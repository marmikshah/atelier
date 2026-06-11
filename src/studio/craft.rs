//! World-class-art tooling — the craft layer on top of the primitives.
//!
//! These methods exist to close the gaps the 30-agent art-quality review found
//! (see docs/ART-QUALITY-REVIEW.md): let the near-blind agent actually SEE
//! (`look`, `select_render`), work without fear (`checkpoint`), edit structure
//! (`layer_ops`, `transform_cel`), and reach perceptual colour & master finish
//! (`make_perceptual_ramp`, `snap_palette`, `smooth_edges`, `select_wand`,
//! `critique`). Image-returning methods hand back raw PNG bytes; the server
//! wraps them as inline MCP image content so the pixels arrive in the same turn.

use std::fs;
use std::path::Path;

use image::{Rgba, RgbaImage};
use serde_json::{json, Value};

use super::{Selection, Studio};
use crate::document::Document;
use crate::raster;

// -- shared raster helpers --------------------------------------------------

/// Encode an image to in-memory PNG bytes.
fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

/// Nearest-neighbour upscale (keeps the pixel grid crisp).
fn scale_nn(img: &RgbaImage, scale: u32) -> RgbaImage {
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

/// Alpha-composite one pixel onto `img` (source-over) — for overlays that must
/// not erase the art underneath.
fn blend_put(img: &mut RgbaImage, x: i32, y: i32, c: [u8; 4]) {
    if x < 0 || y < 0 || x as u32 >= img.width() || y as u32 >= img.height() {
        return;
    }
    let d = img.get_pixel(x as u32, y as u32).0;
    let out = raster::composite_px(d, c, c[3] as f32 / 255.0, raster::Blend::Normal);
    img.put_pixel(x as u32, y as u32, Rgba(out));
}

/// Draw a string with the built-in 3×5 glyph font onto a standalone image
/// (mirror of `Document::text`, but for previews/overlays). Returns the x pen
/// after the last glyph.
fn draw_label(img: &mut RgbaImage, x: i32, y: i32, text: &str, color: [u8; 4]) -> i32 {
    let advance = raster::GLYPH_W + 1;
    let mut pen = x;
    for ch in text.chars() {
        let bits = raster::glyph(ch);
        for gy in 0..raster::GLYPH_H {
            for gx in 0..raster::GLYPH_W {
                let bit = gy * raster::GLYPH_W + (raster::GLYPH_W - 1 - gx);
                if (bits >> bit) & 1 == 1 {
                    blend_put(img, pen + gx, y + gy, color);
                }
            }
        }
        pen += advance;
    }
    pen
}

/// Overlay a pixel-cell grid and coordinate ruler on a scaled preview. `ox,oy`
/// is the native origin of the (possibly cropped) view; `scale` the upscale
/// factor; `step` the native-pixel grid spacing.
fn overlay_grid(img: &mut RgbaImage, ox: i32, oy: i32, scale: u32, step: i32, coords: bool) {
    let step = step.max(1);
    let s = scale as i32;
    let (w, h) = (img.width() as i32, img.height() as i32);
    let line = [255, 0, 255, 70]; // magenta, faint — survives over any art
    // Vertical lines at every `step`-th native column boundary.
    let mut nx = ox - ox.rem_euclid(step);
    while (nx - ox) * s <= w {
        let sx = (nx - ox) * s;
        if sx >= 0 && sx < w {
            for y in 0..h {
                blend_put(img, sx, y, line);
            }
            if coords && nx % (step * 2) == 0 {
                draw_label(img, sx + 1, 0, &nx.to_string(), [255, 255, 0, 220]);
            }
        }
        nx += step;
    }
    let mut ny = oy - oy.rem_euclid(step);
    while (ny - oy) * s <= h {
        let sy = (ny - oy) * s;
        if sy >= 0 && sy < h {
            for x in 0..w {
                blend_put(img, x, sy, line);
            }
            if coords && ny % (step * 2) == 0 {
                draw_label(img, 0, sy + 1, &ny.to_string(), [255, 255, 0, 220]);
            }
        }
        ny += step;
    }
}

/// Crop an image to inclusive native corners (clamped to the canvas). Returns
/// the cropped image and its native origin `(ox, oy)`.
fn crop_region(
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

/// Value-mass + colour stats over the opaque pixels of a native image — the
/// numbers that go beside the inline preview so every look is also measured.
fn look_stats(img: &RgbaImage) -> Value {
    let (mut min, mut max, mut sum, mut n) = (255u8, 0u8, 0u64, 0u64);
    let (mut shadow, mut mid, mut light) = (0u64, 0u64, 0u64);
    let mut distinct = std::collections::HashSet::new();
    for p in img.pixels() {
        if p.0[3] == 0 {
            continue;
        }
        let v = raster::luma(p.0);
        min = min.min(v);
        max = max.max(v);
        sum += v as u64;
        n += 1;
        if v < 85 {
            shadow += 1;
        } else if v < 170 {
            mid += 1;
        } else {
            light += 1;
        }
        distinct.insert([p.0[0], p.0[1], p.0[2], p.0[3]]);
    }
    if n == 0 {
        return json!({"opaque_pixels": 0, "note": "empty — nothing opaque in view"});
    }
    let pct = |c: u64| (c as f64 / n as f64 * 1000.0).round() / 10.0;
    json!({
        "opaque_pixels": n,
        "distinct_colors": distinct.len(),
        "value": {
            "min": min, "max": max,
            "mean": (sum as f64 / n as f64).round() as u32,
            "contrast": ((max - min) as f64 / 255.0 * 1000.0).round() / 1000.0,
        },
        // Value massing: a healthy read groups the canvas into a few clear
        // masses, not a soup. Even thirds is the soup-warning signal.
        "masses_pct": {"shadow": pct(shadow), "mid": pct(mid), "light": pct(light)},
    })
}

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
    // -- doc_look: the single SEE call -------------------------------------

    /// Flatten a frame (or an analysis-space view of it) to inline PNG bytes
    /// plus measured stats — the agent's primary eye. `mode`: `render` |
    /// `value`/`grayscale` | `bands` | `sat` | `hue` | `notan`. `grid`/`coords`
    /// burn a pixel ruler into the upscale. Returns `(png_bytes, stats)`.
    #[allow(clippy::too_many_arguments)]
    pub fn look(
        &self,
        id: &str,
        frame: usize,
        scale: u32,
        region: Option<(i32, i32, i32, i32)>,
        mode: &str,
        bands: u32,
        grid: bool,
        coords: bool,
        onion: bool,
        max_size: Option<u32>,
    ) -> Result<(Vec<u8>, Value), String> {
        let (_dir, doc) = self.open(id)?;
        if frame >= doc.meta.frames.len() {
            return Err(format!("no frame {} (frames={})", frame, doc.meta.frames.len()));
        }
        // Native, full-canvas image for the requested mode.
        let native = match mode {
            "render" => {
                if onion {
                    doc.render_preview(frame, 1, None, true, 1, None)?
                } else {
                    doc.flatten(frame)
                }
            }
            "value" | "grayscale" => doc.value_image(frame, "grayscale", 1)?,
            "bands" => doc.value_image(frame, "bands", bands.max(2))?,
            "sat" | "saturation" => doc.value_image(frame, "saturation", 1)?,
            "hue" => doc.value_image(frame, "hue", 1)?,
            // Notan = 3-value massing (dark / mid / light) — the squint test.
            "notan" => doc.value_image(frame, "bands", 3)?,
            other => {
                return Err(format!(
                    "unknown look mode '{}' — use render|value|bands|sat|hue|notan",
                    other
                ))
            }
        };
        let (view, ox, oy) = crop_region(&native, region)?;
        let stats = look_stats(&view);
        // Scale: explicit nearest upscale, or a max_size thumbnail (no grid).
        let mut out;
        let mut applied_scale = scale.max(1);
        if let Some(ms) = max_size {
            let long = view.width().max(view.height());
            if long > ms && ms > 0 {
                let f = ms as f32 / long as f32;
                out = image::imageops::resize(
                    &view,
                    (view.width() as f32 * f).round().max(1.0) as u32,
                    (view.height() as f32 * f).round().max(1.0) as u32,
                    image::imageops::FilterType::Nearest,
                );
                applied_scale = 1; // thumbnail; grid would be meaningless
            } else {
                out = scale_nn(&view, applied_scale);
            }
        } else {
            out = scale_nn(&view, applied_scale);
        }
        if grid && applied_scale >= 2 {
            // Aim for ~8px native grid cells; at least every pixel boundary.
            let step = (8).min((view.width().max(view.height()) as i32 / 2).max(1));
            overlay_grid(&mut out, ox, oy, applied_scale, step, coords);
        }
        let png = encode_png(&out)?;
        let report = json!({
            "doc_id": id, "frame": frame, "mode": mode,
            "native_size": [view.width(), view.height()],
            "render_size": [out.width(), out.height()],
            "scale": applied_scale,
            "region_origin": [ox, oy],
            "stats": stats,
        });
        Ok((png, report))
    }

    // -- doc_select_render: see the active mask before painting -------------

    /// Render the active selection mask as a quick-mask overlay (selected art
    /// shown, the rest dimmed + tinted) so the agent never paints through an
    /// unseen mask. Returns `(png_bytes, report)`.
    pub fn select_render(&self, id: &str, scale: u32) -> Result<(Vec<u8>, Value), String> {
        let (_dir, doc) = self.open(id)?;
        let (w, h) = (doc.meta.w, doc.meta.h);
        let base = doc.flatten(0);
        let mask = match &self.selection {
            Some(s) if s.doc_id == id && s.w == w && s.h == h => Some(&s.mask),
            _ => None,
        };
        let mut out = RgbaImage::from_pixel(w, h, Rgba([24, 24, 32, 255]));
        let (mut selected, mut bbox): (u64, Option<[i32; 4]>) = (0, None);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let i = (y as u32 * w + x as u32) as usize;
                let sel = mask.map(|m| m.get(i).copied() == Some(true)).unwrap_or(true);
                let art = base.get_pixel(x as u32, y as u32).0;
                let px = if sel {
                    selected += 1;
                    bbox = Some(match bbox {
                        None => [x, y, x, y],
                        Some([a, b, c, d]) => [a.min(x), b.min(y), c.max(x), d.max(y)],
                    });
                    // selected: art over a faint magenta wash
                    let wash = raster::composite_px([40, 12, 40, 255], [255, 0, 255, 40], 40.0 / 255.0, raster::Blend::Normal);
                    raster::composite_px(wash, art, art[3] as f32 / 255.0, raster::Blend::Normal)
                } else {
                    // unselected: dim the art heavily (rubylith feel)
                    let dim = [(art[0] as u32 * 35 / 100) as u8, (art[1] as u32 * 35 / 100) as u8, (art[2] as u32 * 35 / 100) as u8, art[3]];
                    raster::composite_px([24, 24, 32, 255], dim, dim[3] as f32 / 255.0, raster::Blend::Normal)
                };
                out.put_pixel(x as u32, y as u32, Rgba(px));
            }
        }
        let scaled = scale_nn(&out, scale.max(1));
        let png = encode_png(&scaled)?;
        let report = json!({
            "doc_id": id,
            "has_selection": mask.is_some(),
            "selected_pixels": selected,
            "total_pixels": (w * h),
            "bbox": bbox.map(|b| json!({"x": b[0], "y": b[1], "w": b[2]-b[0]+1, "h": b[3]-b[1]+1})),
            "note": if mask.is_none() { "no active selection for this document — showing the whole canvas as selected" } else { "magenta = selected, dimmed = masked out" },
        });
        Ok((png, report))
    }

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
            v.sort();
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
                snapshot_files(&dir, &dst)?;
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
        Ok(json!({"ok": true, "doc_id": id, "action": action, "new_index": new_index, "layers": layers}))
    }

    // -- doc_make_perceptual_ramp ------------------------------------------

    /// Build a perceptually-even shading ramp in OKLCh (the fix for HSL's
    /// crushed midtones). Optionally validate evenness and/or store it as a
    /// document's palette.
    #[allow(clippy::too_many_arguments)]
    pub fn make_perceptual_ramp(
        &self,
        base: [u8; 4],
        count: usize,
        value_lo: Option<f32>,
        value_hi: Option<f32>,
        hue_shift: f32,
        sat_curve: &str,
        anchor_midtone: bool,
        set_doc: Option<&str>,
    ) -> Result<Value, String> {
        let (lb, _, _) = raster::oklab_to_oklch(raster::srgb_to_oklab(base));
        let lo = value_lo.unwrap_or((lb - 0.32).max(0.04));
        let hi = value_hi.unwrap_or((lb + 0.32).min(0.97));
        let ramp = raster::make_ramp_oklch(base, count.max(1), lo, hi, hue_shift, sat_curve, anchor_midtone);
        let hex: Vec<String> = ramp
            .iter()
            .map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]))
            .collect();
        // Validate: perceptual lightness should rise monotonically in even steps.
        let ls: Vec<f32> = ramp.iter().map(|c| raster::srgb_to_oklab(*c).0).collect();
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
            "ramp": ramp, "hex": hex, "count": ramp.len(),
            "validation": {
                "monotonic_lightness": monotonic,
                "mean_step": (mean_step * 1000.0).round() / 1000.0,
                "max_step_deviation": (max_dev * 1000.0).round() / 1000.0,
                "even": max_dev < mean_step.abs() * 0.5 + 0.01,
            }
        });
        if let Some(did) = set_doc {
            self.edit(did, |d| {
                d.set_palette(ramp.clone());
                Ok(())
            })?;
            out["set_doc"] = json!(did);
        }
        Ok(out)
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
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let pal = match palette {
            Some(p) => p,
            None => doc.meta.palette.clone(),
        };
        if pal.is_empty() {
            return Err("no palette to snap to — pass `palette` or set one with doc_set_palette".into());
        }
        let changed = doc.snap_to_palette(&pal, layer, frame);
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "pixels_changed": changed, "palette_len": pal.len()}))
    }

    // -- doc_select_wand: contiguous magic-wand ----------------------------

    /// Flood a contiguous region into the active selection mask (the magic-wand
    /// the roadmap promised). `layer` None samples the flattened composite.
    /// `mode` combines with any current selection: `replace`|`add`|`subtract`|
    /// `intersect`. Perceptual (OKLab) tolerance by default.
    #[allow(clippy::too_many_arguments)]
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
        Ok(json!({"doc_id": id, "selected_pixels": count, "mode": mode, "matched": new.iter().filter(|b| **b).count()}))
    }

    // -- doc_smooth_edges: selective anti-aliasing -------------------------

    /// Selout anti-aliasing of the silhouette's staircase corners (master-grade
    /// smooth diagonals vs Bresenham stairs). `ramp` keeps the AA on-palette.
    #[allow(clippy::too_many_arguments)]
    pub fn smooth_edges(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        ramp: Option<Vec<[u8; 4]>>,
        max_run: i32,
        only_color: Option<[u8; 4]>,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let added = doc.smooth_edges(layer, frame, ramp.as_deref(), max_run, only_color, region)?;
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "aa_pixels_added": added}))
    }

    // -- doc_transform_cel: in-place rotate / scale / skew -----------------

    /// Affine-transform a cel or region in place — the #1 missing primitive.
    /// `method` `rotsprite` (cluster-preserving) | `nearest`. `snap_palette`
    /// re-snaps the transform fringe to the locked palette; `clear_source`
    /// makes it a move rather than an overlay.
    #[allow(clippy::too_many_arguments)]
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
        let (bbox, placed) =
            doc.transform_cel(layer, frame, region, rot, sx, sy, skew_x, skew_y, method, clear_source)?;
        let mut snapped = 0;
        if snap_palette && !doc.meta.palette.is_empty() {
            let pal = doc.meta.palette.clone();
            snapped = doc.snap_to_palette(&pal, Some(layer), Some(frame));
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
    let (mut cxs, mut cys) = (0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            if let Some(p) = op(x, y) {
                let v = raster::luma(p);
                min = min.min(v);
                max = max.max(v);
                sum += v as f64;
                n += 1;
                cxs += x as f64;
                cys += y as f64;
                if v < 85 {
                    shadow += 1;
                } else if v < 170 {
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

    // -- pillow-shading: luma falling radially from the centroid --
    let (mcx, mcy) = (cxs / nf, cys / nf);
    let (mut sr, mut sv, mut srr, mut svv, mut srv) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            if let Some(p) = op(x, y) {
                let r = (((x as f64 - mcx).powi(2)) + ((y as f64 - mcy).powi(2))).sqrt();
                let v = raster::luma(p) as f64;
                sr += r;
                sv += v;
                srr += r * r;
                svv += v * v;
                srv += r * v;
            }
        }
    }
    let cov = srv / nf - (sr / nf) * (sv / nf);
    let vr = (srr / nf - (sr / nf).powi(2)).max(0.0).sqrt();
    let vv = (svv / nf - (sv / nf).powi(2)).max(0.0).sqrt();
    let corr = if vr > 1e-6 && vv > 1e-6 {
        cov / (vr * vv)
    } else {
        0.0
    };
    // negative correlation (bright centre, dark edges, no direction) => pillow
    let pillow = (-corr).max(0.0);

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
            "pillow_shading": {"score": round(pillow), "verdict": if pillow > 0.55 { "warn" } else { "ok" },
                               "note": "high = light pooled at the centre with no direction; shade from a light source instead"},
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

    fn opaque(stats: &Value) -> u64 {
        stats["stats"]["opaque_pixels"].as_u64().unwrap_or(0)
    }

    #[test]
    fn look_returns_png_and_stats() {
        let s = studio("look");
        s.doc_create("c", 8, 8).unwrap();
        s.doc_fill_cel("c", 0, 0, [255, 0, 0, 255]).unwrap();
        let (png, report) = s
            .look("c", 0, 6, None, "render", 4, true, true, false, None)
            .unwrap();
        assert_eq!(&png[0..4], b"\x89PNG");
        assert_eq!(opaque(&report), 64);
        // value mode also works and reports masses
        let (_p, v) = s
            .look("c", 0, 4, None, "value", 4, false, false, false, None)
            .unwrap();
        assert!(v["stats"]["masses_pct"].is_object());
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
            .look("c", 0, 1, None, "render", 4, false, false, false, None)
            .unwrap();
        assert_eq!(opaque(&after.1), 0);
        s.checkpoint("c", "restore", None, Some("cp1")).unwrap();
        let restored = s
            .look("c", 0, 1, None, "render", 4, false, false, false, None)
            .unwrap();
        assert_eq!(opaque(&restored.1), 64);
    }

    #[test]
    fn layer_ops_insert_and_merge() {
        let s = studio("layers");
        s.doc_create("c", 4, 4).unwrap();
        let r = s
            .layer_ops("c", "insert", 0, None, Some("bg".into()), 255, "normal".into())
            .unwrap();
        assert_eq!(r["layers"].as_array().unwrap().len(), 2);
        let m = s
            .layer_ops("c", "merge_down", 1, None, None, 255, "normal".into())
            .unwrap();
        assert_eq!(m["layers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn perceptual_ramp_validates_and_sets_palette() {
        let s = studio("ramp");
        s.doc_create("c", 4, 4).unwrap();
        let r = s
            .make_perceptual_ramp([120, 80, 60, 255], 5, None, None, 20.0, "arc", true, Some("c"))
            .unwrap();
        assert_eq!(r["ramp"].as_array().unwrap().len(), 5);
        assert_eq!(r["validation"]["monotonic_lightness"], true);
        // stored on the doc
        assert_eq!(s.doc_info("c").unwrap()["palette_len"], 5);
    }

    #[test]
    fn snap_palette_moves_off_palette_pixels() {
        let s = studio("snap");
        s.doc_create("c", 4, 4).unwrap();
        s.doc_fill_cel("c", 0, 0, [200, 12, 12, 255]).unwrap();
        s.doc_set_palette("c", vec![[255, 0, 0, 255], [0, 0, 255, 255]])
            .unwrap();
        let r = s.snap_palette("c", None, None, None).unwrap();
        assert_eq!(r["pixels_changed"], 16);
    }

    #[test]
    fn transform_cel_rotate_moves_the_pixel() {
        let s = studio("xform");
        s.doc_create("c", 5, 5).unwrap();
        s.doc_pencil("c", 0, 0, vec![(4, 2)], [255, 255, 255, 255], 1)
            .unwrap();
        let r = s
            .transform_cel("c", 0, 0, None, 90.0, 1.0, 1.0, 0.0, 0.0, "nearest", false, true)
            .unwrap();
        assert_eq!(r["placed_pixels"], 1);
        let look = s
            .look("c", 0, 1, None, "render", 4, false, false, false, None)
            .unwrap();
        assert_eq!(opaque(&look.1), 1); // cleared source, one pixel placed elsewhere
    }

    #[test]
    fn smooth_edges_adds_aa_to_a_staircase() {
        let s = studio("aa");
        s.doc_create("c", 6, 6).unwrap();
        for (x, y) in [(0, 0), (1, 0), (1, 1), (2, 1)] {
            s.doc_pencil("c", 0, 0, vec![(x, y)], [0, 0, 0, 255], 1).unwrap();
        }
        let r = s.smooth_edges("c", 0, 0, None, 2, None, None).unwrap();
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
}

/// Cross-snapshot diff: structural + per-pixel change tallies on frame 0.
fn checkpoint_diff(cpid: &str, was: &Document, now: &Document) -> Value {
    let stat = |d: &Document| -> (u64, usize, u8, u8) {
        let img = d.flatten(0);
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
    let (an, ac, amin, amax) = stat(was);
    let (bn, bc, bmin, bmax) = stat(now);
    // Per-pixel change tally where the canvases line up.
    let (mut added, mut removed, mut changed) = (0u64, 0u64, 0u64);
    if was.meta.w == now.meta.w && was.meta.h == now.meta.h {
        let (ia, ib) = (was.flatten(0), now.flatten(0));
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
