//! World-class-art tooling — the craft layer on top of the primitives.
//!
//! These methods exist to close the gaps the art-quality review found:
//! let the near-blind agent actually SEE
//! (`look`, `select_render`), work without fear (`checkpoint`), edit structure
//! (`layer_ops`, `transform_cel`), and reach perceptual colour & master finish
//! (`palette`, `snap_palette`, `smooth_edges`, `select_wand`,
//! `critique`). Image-returning methods hand back raw PNG bytes; the server
//! wraps them as inline MCP image content so the pixels arrive in the same turn.

use std::fs;
use std::path::Path;

use image::{Rgba, RgbaImage};
use serde_json::{json, Value};

use super::{Selection, Studio};
use atelier_core::document::{Document, Light};
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
                // Clear live cels so checkpoint-absent cels don't linger, then copy back.
                let _ = fs::remove_dir_all(dir.join("cels"));
                let _ = fs::remove_file(dir.join("doc.json"));
                snapshot_files(&cp, &dir)?;
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
            .meta
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
            None => doc.meta.palette.clone(),
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

    // -- doc_select_wand: contiguous magic-wand ----------------------------

    /// Flood a contiguous region into the active selection mask (the magic-wand
    /// the roadmap promised). `layer` None samples the flattened composite.
    /// `mode` combines with any current selection: `replace`|`add`|`subtract`|
    /// `intersect`. Perceptual (OKLab) tolerance by default.
    pub fn select_wand(
        &mut self,
        id: &str,
        layer: Option<usize>,
        frame: usize,
        x: i32,
        y: i32,
        tol: i32,
        conn8: bool,
        perceptual: bool,
        mode: &str,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let (w, h) = (doc.meta.w, doc.meta.h);
        let new = doc.flood_mask(layer, frame, x, y, tol, conn8, perceptual)?;
        let base = match &self.selection {
            Some(s) if s.doc_id == id && s.w == w && s.h == h => s.mask.clone(),
            _ => vec![false; (w * h) as usize],
        };
        let combined: Vec<bool> = (0..base.len())
            .map(|i| {
                let (b, n) = (base[i], new[i]);
                match mode {
                    "add" => b || n,
                    "subtract" => b && !n,
                    "intersect" => b && n,
                    _ => n,
                }
            })
            .collect();
        let count = combined.iter().filter(|b| **b).count();
        self.selection = Some(Selection {
            doc_id: id.to_string(),
            w,
            h,
            mask: combined,
        });
        Ok(
            json!({"doc_id": id, "selected_pixels": count, "mode": mode, "matched": new.iter().filter(|b| **b).count()}),
        )
    }

    // -- doc_smooth_edges: selective anti-aliasing -------------------------

    /// Selout anti-aliasing of the silhouette's staircase corners (master-grade
    /// smooth diagonals vs Bresenham stairs). `ramp` keeps the AA on-palette.
    pub fn smooth_edges(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        ramp: Option<Vec<[u8; 4]>>,
        max_run: i32,
        keep_square: bool,
        only_color: Option<[u8; 4]>,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let added = doc.smooth_edges(
            layer,
            frame,
            ramp.as_deref(),
            max_run,
            keep_square,
            only_color,
            region,
        )?;
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "aa_pixels_added": added}))
    }

    // -- doc_transform_cel: in-place rotate / scale / skew -----------------

    /// Affine-transform a cel or region in place — the #1 missing primitive.
    /// `method` `rotsprite` (cluster-preserving) | `nearest`. `snap_palette`
    /// re-snaps the transform fringe to the locked palette; `clear_source`
    /// makes it a move rather than an overlay.
    pub fn transform_cel(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        rot: f32,
        sx: f32,
        sy: f32,
        skew_x: f32,
        skew_y: f32,
        method: &str,
        snap_palette: bool,
        clear_source: bool,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let (bbox, placed) = doc.transform_cel(
            layer,
            frame,
            region,
            rot,
            sx,
            sy,
            skew_x,
            skew_y,
            method,
            clear_source,
        )?;
        let mut snapped = 0;
        if snap_palette {
            snapped = doc.snap_cel_to_own_palette(
                layer,
                frame,
                atelier_core::document::AlphaSnap::Preserve,
            );
        }
        doc.save(&dir)?;
        Ok(json!({
            "ok": true, "doc_id": id,
            "placed_bbox": {"x": bbox[0], "y": bbox[1], "w": bbox[2], "h": bbox[3]},
            "placed_pixels": placed, "snapped": snapped,
        }))
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
        let palette = doc.meta.palette.clone();
        Ok(critique_image(id, frame, &img, &palette))
    }

    // -- doc_relight: multi-light form shading -----------------------------

    /// Key/fill/rim form shading (the "painted form" leap). Lights are given by
    /// azimuth (0=right, 90=down, 180=left, 270=up) and elevation (0=grazing,
    /// 90=head-on). Fill is auto-placed opposite the key at low elevation. RGB
    /// colours are 0..255. Honours an active selection; `ramp` keeps it on-palette.
    pub fn relight(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        key_az: f32,
        key_elev: f32,
        key_intensity: f32,
        key_color: [u8; 3],
        fill_intensity: f32,
        fill_color: [u8; 3],
        rim_intensity: f32,
        rim_color: [u8; 3],
        ambient: f32,
        ambient_color: [u8; 3],
        bulge: f32,
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<Value, String> {
        let dir = |az: f32, elev: f32| {
            let (a, e) = (az.to_radians(), elev.to_radians());
            [e.cos() * a.cos(), e.cos() * a.sin(), e.sin()]
        };
        let c01 = |x: [u8; 3]| {
            [
                x[0] as f32 / 255.0,
                x[1] as f32 / 255.0,
                x[2] as f32 / 255.0,
            ]
        };
        let mut lights = vec![Light {
            dir: dir(key_az, key_elev),
            intensity: key_intensity,
            color: c01(key_color),
        }];
        if fill_intensity > 0.0 {
            lights.push(Light {
                dir: dir(key_az + 180.0, (key_elev * 0.5).max(8.0)),
                intensity: fill_intensity,
                color: c01(fill_color),
            });
        }
        self.edit_masked(id, layer, frame, |d| {
            d.relight(
                layer,
                frame,
                region,
                &lights,
                ambient,
                c01(ambient_color),
                rim_intensity,
                c01(rim_color),
                bulge,
                ramp.clone(),
            )
        })
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
        let top = vec![(cx, cy - hh), (cx + s, cy), (cx, cy + hh), (cx - s, cy)];
        let left = vec![
            (cx - s, cy),
            (cx, cy + hh),
            (cx, cy + hh + ht),
            (cx - s, cy + ht),
        ];
        let right = vec![
            (cx + s, cy),
            (cx, cy + hh),
            (cx, cy + hh + ht),
            (cx + s, cy + ht),
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

    // -- doc_perspective_guide: non-destructive construction layer ---------

    /// Add a faint guide layer (`thirds` | `grid` | `iso` | `vp`) you can build
    /// against and then delete with doc_layer_ops. `vp` radiates from a
    /// vanishing point; `grid`/`iso` use `spacing`.
    pub fn perspective_guide(
        &self,
        id: &str,
        kind: &str,
        color: [u8; 4],
        spacing: i32,
        vp: Option<(i32, i32)>,
    ) -> Result<Value, String> {
        let sp = spacing.max(2);
        self.edit(id, |d| {
            let li = d.add_layer(Some("guides".into()), 160, "normal".into());
            let (w, h) = (d.meta.w as i32, d.meta.h as i32);
            for f in 0..d.meta.frames.len() {
                match kind {
                    "thirds" => {
                        for k in 1..3 {
                            let x = w * k / 3;
                            d.line(li, f, x, 0, x, h - 1, color, 1)?;
                            let y = h * k / 3;
                            d.line(li, f, 0, y, w - 1, y, color, 1)?;
                        }
                    }
                    "grid" => {
                        let mut x = 0;
                        while x < w {
                            d.line(li, f, x, 0, x, h - 1, color, 1)?;
                            x += sp;
                        }
                        let mut y = 0;
                        while y < h {
                            d.line(li, f, 0, y, w - 1, y, color, 1)?;
                            y += sp;
                        }
                    }
                    "iso" => {
                        let mut b = -w;
                        while b < h + w {
                            // 2:1 iso lines both ways
                            d.line(li, f, 0, b, w - 1, b + (w - 1) / 2, color, 1)?;
                            d.line(li, f, 0, b, w - 1, b - (w - 1) / 2, color, 1)?;
                            b += sp;
                        }
                    }
                    "vp" => {
                        let (vx, vy) = vp.unwrap_or((w / 2, 0));
                        // radiate to evenly spaced points around the border
                        let steps = 24;
                        for i in 0..steps {
                            let t = i as f32 / steps as f32 * 4.0;
                            let (ex, ey) = match t as i32 {
                                0 => ((t.fract() * w as f32) as i32, 0),
                                1 => (w - 1, (t.fract() * h as f32) as i32),
                                2 => (((1.0 - t.fract()) * (w - 1) as f32) as i32, h - 1),
                                _ => (0, ((1.0 - t.fract()) * h as f32) as i32),
                            };
                            d.line(li, f, vx, vy, ex, ey, color, 1)?;
                        }
                    }
                    other => {
                        return Err(format!(
                            "unknown guide kind '{}' — use thirds|grid|iso|vp",
                            other
                        ))
                    }
                }
            }
            Ok(())
        })
    }

    // -- doc_outline_selective: form-following contour ---------------------

    /// Form-following selective outline (vs a flat black keyline). `mode`
    /// `from_fill` colours each edge from the fill it borders; `light`/`dark`
    /// bias it. `ramp` keeps it on-palette.
    pub fn outline_selective(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        mode: &str,
        ramp: Option<Vec<[u8; 4]>>,
        steps: i32,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let n = doc.outline_selective(layer, frame, mode, ramp.as_deref(), steps, region)?;
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "outline_pixels": n}))
    }

    // -- doc_material: procedural material recipes -------------------------

    /// Paint a procedural material onto the opaque pixels of a cel — metal,
    /// wood, stone, water, cloth, skin, glass — derived from one base colour (or
    /// an explicit `ramp`). Honours an active selection so it clings to a
    /// selected shape. Snap afterwards if it drifts.
    pub fn material(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        material: &str,
        base: [u8; 4],
        seed: u64,
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<Value, String> {
        let ramp = ramp
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| auto_ramp(base, 6));
        self.edit_masked(id, layer, frame, |d| {
            d.material(layer, frame, region, material, &ramp, seed)
                .map(|_| ())
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
        let (x1, y1) = (x + w - 1, y + h - 1);
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

    /// TRUE 9-slice: author a panel once, emit it at ANY size. The `src`
    /// region (on `src_layer`/`src_frame`) is cut into a 3×3 grid by `inset`:
    /// corners copy verbatim, edges and the centre tile (`mode="tile"`) or
    /// stretch (`"stretch"`) to fill the `dst` rect on `layer`/`frame`.
    /// Transparent source pixels are skipped, so a rounded panel keeps its
    /// shape over existing art.
    pub fn nine_slice(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        src_layer: usize,
        src_frame: usize,
        src: (i32, i32, i32, i32),
        inset: i32,
        dst: (i32, i32, i32, i32),
        mode: &str,
    ) -> Result<Value, String> {
        let (sx, sy, sw, sh) = src;
        let (dx, dy, dw, dh) = dst;
        let b = inset.max(1);
        if sw < 2 * b + 1 || sh < 2 * b + 1 {
            let min = 2 * b + 1;
            return Err(format!(
                "source {sw}x{sh} too small for inset {b} — needs at least {min}x{min}"
            ));
        }
        if dw < 2 * b || dh < 2 * b {
            return Err(format!(
                "dest {}x{} smaller than the corners (2×inset {})",
                dw,
                dh,
                2 * b
            ));
        }
        if !matches!(mode, "tile" | "stretch") {
            return Err(format!("unknown mode '{mode}' — use tile|stretch"));
        }
        let (dir, mut doc) = self.open(id)?;
        let src_img = doc.analysis_image(Some(src_layer), src_frame)?;
        let (cw, ch) = (doc.meta.w as i32, doc.meta.h as i32);
        // Map one dest axis offset to a source axis offset.
        let map_axis = |d: i32, dlen: i32, slen: i32| -> i32 {
            if d < b {
                d // leading corner: verbatim
            } else if d >= dlen - b {
                slen - (dlen - d) // trailing corner: verbatim from the far side
            } else {
                let span_s = slen - 2 * b;
                let span_d = dlen - 2 * b;
                match mode {
                    "tile" => b + (d - b) % span_s,
                    _ => b + ((d - b) as i64 * span_s as i64 / span_d.max(1) as i64) as i32,
                }
            }
        };
        // Build the whole panel as a patch, then stamp it over the target cel.
        let mut patch = RgbaImage::from_pixel(dw as u32, dh as u32, image::Rgba([0, 0, 0, 0]));
        let mut placed = 0u32;
        for oy in 0..dh {
            for ox in 0..dw {
                let (mx, my) = (map_axis(ox, dw, sw), map_axis(oy, dh, sh));
                let (gx, gy) = (sx + mx, sy + my);
                if gx < 0 || gy < 0 || gx >= cw || gy >= ch {
                    continue;
                }
                let p = src_img.get_pixel(gx as u32, gy as u32).0;
                if p[3] == 0 {
                    continue;
                }
                patch.put_pixel(ox as u32, oy as u32, image::Rgba(p));
                placed += 1;
            }
        }
        doc.stamp_image(layer, frame, dx, dy, patch, 1.0, 0.0, 255, "normal", false)?;
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "pixels_placed": placed}))
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
        let palette: Vec<[u8; 4]> = if to_doc_palette && !doc.meta.palette.is_empty() {
            doc.meta.palette.clone()
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
        if to_doc_palette && doc.meta.palette.is_empty() {
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

    // -- doc_burst: radial FX across frames -------------------------------

    /// Generate a radial FX animation (ring | disc | rays) expanding from a
    /// centre across `frames`, fading along the ramp, tagged `burst`. VFX-as-
    /// frames: impacts, shockwaves, explosions. Clears the target layer's cels.
    pub fn burst(
        &self,
        id: &str,
        layer: usize,
        cx: i32,
        cy: i32,
        frames: usize,
        max_radius: i32,
        kind: &str,
        base: [u8; 4],
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<Value, String> {
        let frames = frames.max(2);
        let (dir, mut doc) = self.open(id)?;
        if layer >= doc.meta.layers.len() {
            return Err(format!("no layer {}", layer));
        }
        let ramp = ramp
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| auto_ramp(base, frames.clamp(2, 8)));
        while doc.meta.frames.len() < frames {
            doc.add_frame(80, None);
        }
        let last = ramp.len() - 1;
        for f in 0..frames {
            let t = f as f32 / (frames - 1) as f32;
            let r = (t * max_radius as f32).round().max(1.0) as i32;
            doc.clear_cel(layer, f);
            // Expand-and-dissipate: bright and solid at the flash, thinning to a
            // faint rim as it grows — the shockwave fades OUT, instead of
            // darkening while staying fully opaque (which read as a solid ring).
            let c = ramp[(((1.0 - t) * last as f32).round() as usize).min(last)];
            let a = ((1.0 - 0.8 * t) * c[3] as f32).round().clamp(0.0, 255.0) as u8;
            let col = [c[0], c[1], c[2], a];
            match kind {
                "ring" => doc.ellipse(layer, f, cx, cy, r, r, col, false)?,
                "disc" => doc.ellipse(layer, f, cx, cy, r, r, col, true)?,
                "rays" => {
                    for a in (0..360).step_by(30) {
                        let rad = (a as f32).to_radians();
                        let ex = cx + (r as f32 * rad.cos()) as i32;
                        let ey = cy + (r as f32 * rad.sin()) as i32;
                        doc.line(layer, f, cx, cy, ex, ey, col, 1)?;
                    }
                }
                other => {
                    return Err(format!(
                        "unknown burst kind '{}' — use ring|disc|rays",
                        other
                    ))
                }
            }
        }
        if !doc.meta.tags.iter().any(|t| t.name == "burst") {
            doc.add_tag("burst", 0, frames - 1, "forward")?;
        }
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "frames": frames, "kind": kind}))
    }

    /// Seeded PARTICLE EMITTER rendered to frames — sparks, embers, smoke,
    /// rain, magic motes. Every particle's whole trajectory is a pure function
    /// of (seed, particle index): spawned inside the emitter rect with a
    /// direction in `angle ± spread`, advanced by `speed` and `gravity`, faded
    /// and shrunk along its `life`, coloured along the ramp — so the animation
    /// is deterministic and loops cleanly when `loop_seam` (particles respawn
    /// with staggered phase). Draws onto `layer` across `frames`, clearing each
    /// cel, and tags the range `emit`.
    pub fn emit(
        &self,
        id: &str,
        layer: usize,
        region: (i32, i32, i32, i32),
        frames: usize,
        count: usize,
        angle_deg: f32,
        spread_deg: f32,
        speed: f32,
        gravity: f32,
        life: f32,
        size: i32,
        seed: u64,
        base: [u8; 4],
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<Value, String> {
        let frames = frames.clamp(2, 24);
        let count = count.clamp(1, 512);
        let (ex, ey, ew, eh) = region;
        if ew < 1 || eh < 1 {
            return Err("emitter region must be at least 1x1".into());
        }
        let (dir, mut doc) = self.open(id)?;
        if layer >= doc.meta.layers.len() {
            return Err(format!("no layer {}", layer));
        }
        let ramp = ramp
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| auto_ramp(base, 5));
        while doc.meta.frames.len() < frames {
            doc.add_frame(80, None);
        }
        let life = life.clamp(0.2, 4.0); // in cycle units: 1.0 = one full loop
        let last = ramp.len() - 1;
        // A unit random in [0,1) that is pure in (seed, particle, channel).
        let rnd = |p: usize, ch: i32| raster::hash2(p as i32, ch, seed) as f32 / u32::MAX as f32;
        for f in 0..frames {
            doc.clear_cel(layer, f);
            let ft = f as f32 / frames as f32;
            for p in 0..count {
                // Staggered phase: each particle is somewhere along its own
                // life when the clip starts, so the loop has no "big bang".
                let phase = (ft / life + rnd(p, 0)).fract();
                let age = phase * life; // 0..life, in cycle units
                let a0 = (angle_deg + spread_deg * (rnd(p, 1) * 2.0 - 1.0)).to_radians();
                let v = speed * (0.6 + 0.4 * rnd(p, 2));
                let sx = ex as f32 + rnd(p, 3) * ew as f32;
                let sy = ey as f32 + rnd(p, 4) * eh as f32;
                // Analytic ballistic position at this age (frames of travel).
                let tt = age * frames as f32;
                let px = sx + a0.cos() * v * tt;
                let py = sy + a0.sin() * v * tt + 0.5 * gravity * tt * tt;
                let lt = phase; // 0 = just born, 1 = dying
                let c = ramp[((lt * last as f32).round() as usize).min(last)];
                let alpha = ((1.0 - lt) * c[3] as f32).round().clamp(0.0, 255.0) as u8;
                if alpha == 0 {
                    continue;
                }
                // Particles shrink as they die.
                let r = ((size as f32) * (1.0 - 0.6 * lt)).max(0.5) / 2.0;
                doc.stroke_f(
                    layer,
                    f,
                    &[(px, py, r * 2.0)],
                    [c[0], c[1], c[2], alpha],
                    true,
                    false,
                )?;
            }
            doc.snap_cel_to_own_palette(layer, f, atelier_core::document::AlphaSnap::Preserve);
        }
        if !doc.meta.tags.iter().any(|t| t.name == "emit") {
            doc.add_tag("emit", 0, frames - 1, "forward")?;
        }
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "frames": frames, "particles": count}))
    }
}

/// A perceptually-even ramp bracketing a base colour's lightness — the default
/// ramp shared by `box_iso` and `material`.
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
               "note": "exact-match check; soft FX bloom counts as off-palette — snap with doc_snap_palette if undeliberate"})
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
            "orphans": {"count": orphans, "verdict": if orphans > 0 { "warn" } else { "ok" }, "cells": orphan_cells},
            "jaggies": {"count": jaggies, "verdict": if jaggies > (n / 12).max(6) as u32 { "warn" } else { "info" },
                        "cells": jag_cells, "note": "outer step corners; run doc_smooth_edges to selout them"},
            "pillow_shading": {"forms": pillow_forms, "verdict": if pillow_forms > 0 { "warn" } else { "ok" },
                               "note": "forms lit brightest at the centre with no light direction; shade from a light source (doc_form_audit has the per-form breakdown)"},
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
    if was.meta.w == now.meta.w && was.meta.h == now.meta.h {
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

    fn opaque(stats: &Value) -> u64 {
        stats["stats"]["opaque_pixels"].as_u64().unwrap_or(0)
    }

    #[test]
    fn nine_slice_keeps_corners_and_fills_centre() {
        let s = studio("nine");
        s.doc_create("ui", 48, 48).unwrap();
        // Author a 9×9 panel: red border, blue fill, at (0,0).
        s.panel(
            "ui",
            0,
            0,
            0,
            0,
            9,
            9,
            [40, 60, 220, 255],
            [220, 40, 40, 255],
            false,
        )
        .unwrap();
        // Emit it at 24×12 lower on the canvas.
        let r = s
            .nine_slice("ui", 0, 0, 0, 0, (0, 0, 9, 9), 3, (4, 20, 24, 12), "tile")
            .unwrap();
        assert!(r["pixels_placed"].as_u64().unwrap() > 0);
        let px = |x: i32, y: i32| s.doc_get_pixel("ui", Some(0), 0, x, y).unwrap()["rgba"].clone();
        // All four dest corners carry the border colour...
        for (x, y) in [(4, 20), (27, 20), (4, 31), (27, 31)] {
            assert_eq!(px(x, y), json!([220, 40, 40, 255]), "corner {x},{y}");
        }
        // ...edges too (top mid), and the centre is fill.
        assert_eq!(px(16, 20), json!([220, 40, 40, 255]));
        assert_eq!(px(16, 26), json!([40, 60, 220, 255]));
        // Too-small dest errors.
        assert!(s
            .nine_slice("ui", 0, 0, 0, 0, (0, 0, 9, 9), 3, (0, 0, 5, 5), "tile")
            .is_err());
    }

    #[test]
    fn emit_is_deterministic_and_loops() {
        let s = studio("emit");
        for d in ["fx-a", "fx-b"] {
            s.doc_create(d, 32, 32).unwrap();
            s.emit(
                d,
                0,
                (12, 24, 8, 4),
                6,
                16,
                270.0,
                25.0,
                1.5,
                0.0,
                1.0,
                2,
                7,
                [255, 180, 60, 255],
                None,
            )
            .unwrap();
        }
        // Same seed ⇒ byte-identical frames across separate runs (sampled grid
        // keeps the test fast; determinism failures are not localized anyway).
        let mut any_opaque = false;
        for f in 0..6 {
            for y in (0..32).step_by(3) {
                for x in (0..32).step_by(3) {
                    let a = s.doc_get_pixel("fx-a", Some(0), f, x, y).unwrap()["rgba"].clone();
                    let b = s.doc_get_pixel("fx-b", Some(0), f, x, y).unwrap()["rgba"].clone();
                    assert_eq!(a, b, "frame {f} pixel {x},{y}");
                    if a[3] != json!(0) {
                        any_opaque = true;
                    }
                }
            }
        }
        assert!(any_opaque, "emitter drew nothing in the sampled grid");
    }

    #[test]
    fn checkpoint_save_restore_round_trips() {
        let s = studio("cp");
        s.doc_create("c", 8, 8).unwrap();
        s.doc_fill_cel("c", 0, 0, [0, 200, 0, 255]).unwrap();
        let saved = s.checkpoint("c", "save", Some("base"), None).unwrap();
        assert_eq!(saved["saved"], "cp1");
        s.doc_clear_cel("c", 0, 0).unwrap();
        let after = s
            .look(
                "c",
                0,
                Some(1),
                None,
                "render",
                4,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(opaque(&after.1), 0);
        s.checkpoint("c", "restore", None, Some("cp1")).unwrap();
        let restored = s
            .look(
                "c",
                0,
                Some(1),
                None,
                "render",
                4,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(opaque(&restored.1), 64);
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
    fn snap_palette_moves_off_palette_pixels() {
        let s = studio("snap");
        s.doc_create("c", 4, 4).unwrap();
        s.doc_fill_cel("c", 0, 0, [200, 12, 12, 255]).unwrap();
        s.doc_set_palette("c", vec![[255, 0, 0, 255], [0, 0, 255, 255]])
            .unwrap();
        let r = s
            .snap_palette(
                "c",
                None,
                None,
                None,
                atelier_core::document::AlphaSnap::Preserve,
            )
            .unwrap();
        assert_eq!(r["pixels_changed"], 16);
    }

    #[test]
    fn transform_cel_rotate_moves_the_pixel() {
        let s = studio("xform");
        s.doc_create("c", 5, 5).unwrap();
        s.doc_pencil("c", 0, 0, vec![(4, 2)], [255, 255, 255, 255], 1)
            .unwrap();
        let r = s
            .transform_cel(
                "c", 0, 0, None, 90.0, 1.0, 1.0, 0.0, 0.0, "nearest", false, true,
            )
            .unwrap();
        assert_eq!(r["placed_pixels"], 1);
        let look = s
            .look(
                "c",
                0,
                Some(1),
                None,
                "render",
                4,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(opaque(&look.1), 1); // cleared source, one pixel placed elsewhere
    }

    #[test]
    fn smooth_edges_adds_aa_to_a_staircase() {
        let s = studio("aa");
        s.doc_create("c", 6, 6).unwrap();
        for (x, y) in [(0, 0), (1, 0), (1, 1), (2, 1)] {
            s.doc_pencil("c", 0, 0, vec![(x, y)], [0, 0, 0, 255], 1)
                .unwrap();
        }
        let r = s
            .smooth_edges("c", 0, 0, None, 2, true, None, None)
            .unwrap();
        assert!(r["aa_pixels_added"].as_u64().unwrap() >= 2);
    }

    #[test]
    fn select_wand_floods_a_solid_cel() {
        let mut s = studio("wand");
        s.doc_create("c", 4, 4).unwrap();
        s.doc_fill_cel("c", 0, 0, [10, 10, 10, 255]).unwrap();
        let r = s
            .select_wand("c", Some(0), 0, 0, 0, 8, false, true, "replace")
            .unwrap();
        assert_eq!(r["selected_pixels"], 16);
    }

    #[test]
    fn critique_flags_an_orphan_speck() {
        let s = studio("crit");
        s.doc_create("c", 8, 8).unwrap();
        s.doc_pencil("c", 0, 0, vec![(1, 1)], [255, 255, 255, 255], 1)
            .unwrap();
        let r = s.critique("c", 0, None, None).unwrap();
        assert_eq!(r["checks"]["orphans"]["count"], 1);
    }

    #[test]
    fn critique_flags_pillow_shading_via_form_audit() {
        let s = studio("critpillow");
        s.doc_create("p", 16, 16).unwrap();
        // Concentric squares: bright centre, dark all edges — pillow-shaded, no
        // light direction. The form-audit-backed check should warn.
        s.doc_rect("p", 0, 0, 2, 2, 13, 13, [50, 50, 60, 255], true, 1)
            .unwrap();
        s.doc_rect("p", 0, 0, 4, 4, 11, 11, [90, 90, 105, 255], true, 1)
            .unwrap();
        s.doc_rect("p", 0, 0, 6, 6, 9, 9, [140, 140, 160, 255], true, 1)
            .unwrap();
        s.doc_rect("p", 0, 0, 7, 7, 8, 8, [200, 200, 225, 255], true, 1)
            .unwrap();
        let r = s.critique("p", 0, None, None).unwrap();
        assert_eq!(r["checks"]["pillow_shading"]["verdict"], "warn");
        assert!(r["checks"]["pillow_shading"]["forms"].as_u64().unwrap() >= 1);
    }

    fn distinct(stats: &Value) -> u64 {
        stats["stats"]["distinct_colors"].as_u64().unwrap_or(0)
    }

    #[test]
    fn relight_shades_a_flat_fill_into_form() {
        let s = studio("relight");
        s.doc_create("c", 16, 16).unwrap();
        s.doc_fill_cel("c", 0, 0, [128, 128, 128, 255]).unwrap();
        s.relight(
            "c",
            0,
            0,
            None,
            315.0,
            50.0,
            1.0,
            [255, 255, 255],
            0.25,
            [120, 140, 200],
            0.0,
            [255, 255, 255],
            0.3,
            [120, 130, 170],
            2.0,
            None,
        )
        .unwrap();
        let look = s
            .look(
                "c",
                0,
                Some(1),
                None,
                "render",
                1,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(
            distinct(&look.1) > 1,
            "relight should produce a value gradient"
        );
    }

    #[test]
    fn dither_ramp_spreads_across_the_ramp() {
        let s = studio("dramp");
        s.doc_create("c", 4, 8).unwrap();
        s.doc_fill_cel("c", 0, 0, [100, 100, 100, 255]).unwrap();
        s.dither_ramp(
            "c",
            0,
            0,
            None,
            vec![[0, 0, 0, 255], [128, 128, 128, 255], [255, 255, 255, 255]],
            "v",
            "bayer4",
            true,
        )
        .unwrap();
        let look = s
            .look(
                "c",
                0,
                Some(1),
                None,
                "render",
                1,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(distinct(&look.1) >= 2);
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
                Some(1),
                None,
                "render",
                1,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(distinct(&look.1) >= 3, "three faces => three shades");
    }

    #[test]
    fn material_paints_only_opaque_pixels() {
        let s = studio("material");
        s.doc_create("c", 16, 16).unwrap();
        s.doc_pencil("c", 0, 0, vec![(4, 4)], [120, 120, 120, 255], 1)
            .unwrap();
        s.material("c", 0, 0, None, "metal", [120, 120, 130, 255], 1, None)
            .unwrap();
        let look = s
            .look(
                "c",
                0,
                Some(1),
                None,
                "render",
                1,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .unwrap();
        // still exactly one opaque pixel — material clings to the shape
        assert_eq!(opaque(&look.1), 1);
    }

    #[test]
    fn outline_selective_rings_a_shape() {
        let s = studio("outsel");
        s.doc_create("c", 8, 8).unwrap();
        s.doc_rect("c", 0, 0, 2, 2, 5, 5, [200, 60, 60, 255], true, 1)
            .unwrap();
        let r = s
            .outline_selective("c", 0, 0, "from_fill", None, 2, None)
            .unwrap();
        assert!(r["outline_pixels"].as_u64().unwrap() > 0);
    }

    #[test]
    fn perspective_guide_adds_a_layer() {
        let s = studio("guide");
        s.doc_create("c", 24, 24).unwrap();
        s.perspective_guide("c", "thirds", [255, 0, 255, 130], 8, None)
            .unwrap();
        assert_eq!(
            s.doc_info("c").unwrap()["layers"].as_array().unwrap().len(),
            2
        );
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
                Some(1),
                None,
                "render",
                1,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(distinct(&look.1) >= 2);
    }

    #[test]
    fn burst_creates_frames_and_tag() {
        let s = studio("burst");
        s.doc_create("c", 16, 16).unwrap();
        s.burst("c", 0, 8, 8, 5, 7, "ring", [255, 200, 60, 255], None)
            .unwrap();
        let info = s.doc_info("c").unwrap();
        assert!(info["frames"].as_array().unwrap().len() >= 5);
        assert!(info["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "burst"));
    }

    #[test]
    fn empty_ramp_falls_back_to_auto_ramp_instead_of_panicking() {
        let s = studio("emptyramp");
        s.doc_create("c", 16, 16).unwrap();
        s.burst(
            "c",
            0,
            8,
            8,
            5,
            7,
            "ring",
            [255, 200, 60, 255],
            Some(vec![]),
        )
        .unwrap();
        s.emit(
            "c",
            0,
            (4, 4, 8, 8),
            4,
            8,
            90.0,
            30.0,
            1.0,
            0.0,
            1.0,
            1,
            7,
            [200, 220, 255, 255],
            Some(vec![]),
        )
        .unwrap();
        s.doc_fill_cel("c", 0, 0, [90, 90, 90, 255]).unwrap();
        s.material("c", 0, 0, None, "stone", [90, 90, 90, 255], 7, Some(vec![]))
            .unwrap();
    }

    #[test]
    fn translucency_report_counts_partial_alpha() {
        let s = studio("trans");
        s.doc_create("c", 4, 4).unwrap();
        s.doc_fill_cel("c", 0, 0, [255, 255, 255, 128]).unwrap();
        let r = s.doc_translucency_report("c", 0, None, None).unwrap();
        assert_eq!(r["partial"], 16);
    }

    #[test]
    fn anim_audit_arc_reports_trajectory_shape() {
        let s = studio("arc");
        s.doc_create("c", 16, 16).unwrap();
        s.doc_add_frame("c", 100, None).unwrap();
        s.doc_add_frame("c", 100, None).unwrap();
        s.doc_pencil("c", 0, 0, vec![(2, 8)], [255, 255, 255, 255], 1)
            .unwrap();
        s.doc_pencil("c", 0, 1, vec![(8, 2)], [255, 255, 255, 255], 1)
            .unwrap();
        s.doc_pencil("c", 0, 2, vec![(14, 8)], [255, 255, 255, 255], 1)
            .unwrap();
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
                Some(1),
                None,
                "render",
                1,
                false,
                false,
                false,
                None,
                None,
                None,
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
}
