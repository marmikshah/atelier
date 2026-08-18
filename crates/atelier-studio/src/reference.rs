//! Reference-image subsystem: store the original a document is recreating,
//! view/analyze it through atelier's eye, and SCORE the canvas against it —
//! the closed loop that turns "does my sprite match the character?" from
//! cross-turn memory recall into a measurable, same-turn signal.

use std::path::Path;

use image::{Rgba, RgbaImage};
use serde_json::{Value, json};

use super::{CompareMode, Studio, encode_png, preview_scale};
use atelier_core::document::Document;
use atelier_core::raster;

/// OKLab ΔE used for corner-flood background detection on references.
const BG_TOL: f32 = 0.08;
/// A reference colour with no doc colour within this ΔE counts as missing.
const MISSING_TOL: f32 = 0.12;
const MAX_SUBJECT_PALETTE_COLORS: usize = 256;

impl Studio {
    /// Attach a reference image to a document: the file is decoded (validation),
    /// re-encoded as PNG into the doc dir, and recorded in the meta so it
    /// persists with the document. `path` None clears the reference.
    pub fn set_reference(&self, id: &str, path: Option<&str>) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let Some(path) = path else {
            if let Some(name) = doc.set_reference_file(None)
                && let Ok(p) = ref_path(&dir, &name)
            {
                let _ = std::fs::remove_file(p);
            }
            doc.save(&dir)?;
            return Ok(json!({"ok": true, "doc_id": id, "reference": Value::Null}));
        };
        let img = crate::open_bounded(Path::new(path))?;
        let (rw, rh) = (img.width(), img.height());
        // Replace instead of truncating the live inode. Store transactions
        // hard-link unchanged files into their staging tree; a temp+rename
        // keeps the live generation untouched until the transaction commits.
        let reference = dir.join("reference.png");
        let temporary = dir.join("reference.png.tmp");
        let write = img
            .save_with_format(&temporary, image::ImageFormat::Png)
            .map_err(|e| e.to_string())
            .and_then(|()| std::fs::rename(&temporary, &reference).map_err(|e| e.to_string()));
        if let Err(error) = write {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        doc.set_reference_file(Some("reference.png".into()));
        doc.save(&dir)?;
        let (cw, ch) = (doc.meta().w, doc.meta().h);
        // Aspect-true size suggestions at the canvas dims, so the agent sees a
        // mismatch BEFORE drawing into it.
        let fit_w = ((rw as f64 * ch as f64 / rh as f64).round() as u32).max(1);
        let fit_h = ((rh as f64 * cw as f64 / rw as f64).round() as u32).max(1);
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "reference": "reference.png",
            "source_size": [rw, rh],
            "canvas_size": [cw, ch],
            "aspect_true_fit": {"at_canvas_width": [cw, fit_h], "at_canvas_height": [fit_w, ch]},
        }))
    }

    /// Load a document's stored reference image.
    fn ref_image(dir: &Path, doc: &Document) -> Result<RgbaImage, String> {
        let name = doc
            .meta()
            .reference
            .as_ref()
            .ok_or("document has no reference image — use doc_ref op=set first")?;
        crate::open_bounded(&ref_path(dir, name)?).map_err(|e| format!("reference unreadable: {e}"))
    }

    /// The shared compare prelude: flatten frame `frame`, load the stored
    /// reference, strip its backdrop and area-downscale it to canvas size —
    /// the (canvas, reference-at-canvas-size, doc) every scorer starts from.
    fn ref_vs_canvas(
        &self,
        id: &str,
        frame: usize,
    ) -> Result<(RgbaImage, RgbaImage, Document), String> {
        let (dir, doc) = self.open(id)?;
        let src = Self::ref_image(&dir, &doc)?;
        let canvas = doc.analysis_image(None, frame)?;
        let mut subject = src.clone();
        raster::remove_background(&mut subject, BG_TOL);
        let small = raster::scale(
            &subject,
            canvas.width(),
            canvas.height(),
            raster::ScaleMethod::AreaAverage,
        );
        Ok((canvas, small, doc))
    }

    /// Analyze the reference (or an external `path`) as drawing scaffolding:
    /// view it inline, plus background coverage, a frequency-weighted subject
    /// palette, and a silhouette grid at the target size — what an agent needs
    /// to PLAN a recreation instead of freehanding from memory.
    pub fn ref_analyze(
        &self,
        id: &str,
        path: Option<&str>,
        target_w: Option<u32>,
        colors: usize,
    ) -> Result<(Vec<u8>, Value), String> {
        if !(2..=MAX_SUBJECT_PALETTE_COLORS).contains(&colors) {
            return Err(format!(
                "reference palette colors must be 2..={MAX_SUBJECT_PALETTE_COLORS}, got {colors}"
            ));
        }
        let (dir, doc) = self.open(id)?;
        let src = match path {
            Some(p) => crate::open_bounded(Path::new(p))?,
            None => Self::ref_image(&dir, &doc)?,
        };
        let (rw, rh) = (src.width(), src.height());
        // Subject = source minus corner-flooded backdrop (non-destructive copy).
        let mut subject = src.clone();
        raster::remove_background(&mut subject, BG_TOL);
        let total = (rw * rh) as u64;
        let cleared = subject.pixels().filter(|p| p.0[3] == 0).count() as u64
            - src.pixels().filter(|p| p.0[3] == 0).count() as u64;
        let tw = target_w.unwrap_or(doc.meta().w).max(1);
        let th = ((rh as f64 * tw as f64 / rw.max(1) as f64).round() as u32).max(1);
        // An oversized target_w would otherwise allocate an unbounded image in
        // one call.
        if tw as usize * th as usize > crate::MAX_TARGET_PIXELS {
            return Err(format!(
                "target {}x{} is over the 1M-pixel cap — pass a smaller target_w",
                tw, th
            ));
        }
        let small = raster::scale(&subject, tw, th, raster::ScaleMethod::AreaAverage);
        // Frequency-weighted subject palette of the downscaled art.
        let palette = subject_palette(&small, colors);
        let pal_json: Vec<Value> = palette.iter().map(|c| json!(c)).collect();
        // Silhouette grid at target size (capped so the text stays readable).
        let silhouette: Value = if ((tw * th) as u64) <= crate::GRID_AREA_CAP {
            let rows: Vec<String> = (0..th)
                .map(|y| {
                    (0..tw)
                        .map(|x| {
                            if small.get_pixel(x, y).0[3] >= 128 {
                                '#'
                            } else {
                                '.'
                            }
                        })
                        .collect()
                })
                .collect();
            json!(rows)
        } else {
            json!(format!(
                "skipped — {}x{} over the 4096-px grid cap; pass a smaller target_w",
                tw, th
            ))
        };
        // Inline view: the raw reference, fitted under ~384px.
        let png = encode_png(&fit_under(&src, 384))?;
        Ok((
            png,
            json!({
                "source_size": [rw, rh],
                "suggested_target": [tw, th],
                "bg_coverage_pct": (cleared as f64 / total.max(1) as f64 * 100.0).round(),
                "subject_palette": pal_json,
                "silhouette": silhouette,
            }),
        ))
    }

    /// Score frame `frame` against the stored reference: silhouette IoU,
    /// per-cell OKLab ΔE with the worst cells called out as canvas
    /// coordinates, and reference colours missing from the doc palette —
    /// plus an inline side-by-side (or overlay) so the agent SEES the gap it
    /// is closing. The render loop for "make it look like the sample".
    pub fn ref_compare(
        &self,
        id: &str,
        frame: usize,
        mode: CompareMode,
        cells: u32,
    ) -> Result<(Vec<u8>, Value), String> {
        let (canvas, small, doc) = self.ref_vs_canvas(id, frame)?;
        let (cw, ch) = (canvas.width(), canvas.height());
        // Silhouette IoU over alpha masks.
        let (mut inter, mut union) = (0u64, 0u64);
        for (a, b) in canvas.pixels().zip(small.pixels()) {
            let (ca, ra) = (a.0[3] >= 128, b.0[3] >= 128);
            if ca && ra {
                inter += 1;
            }
            if ca || ra {
                union += 1;
            }
        }
        let iou = if union == 0 {
            0.0
        } else {
            inter as f64 / union as f64
        };
        // Per-cell mean ΔE where both are opaque; worst cells as coordinates.
        let cells = cells.clamp(2, 16);
        let (cw_px, ch_px) = (cw.div_ceil(cells).max(1), ch.div_ceil(cells).max(1));
        let mut cell_stats: Vec<(f64, [u32; 4])> = Vec::new();
        let mut total_delta = 0f64;
        let mut total_n = 0u64;
        for cy in 0..cells {
            for cx in 0..cells {
                let (x0, y0) = (cx * cw_px, cy * ch_px);
                if x0 >= cw || y0 >= ch {
                    continue;
                }
                let (x1, y1) = ((x0 + cw_px).min(cw), (y0 + ch_px).min(ch));
                let (mut sum, mut n) = (0f64, 0u64);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (a, b) = (canvas.get_pixel(x, y).0, small.get_pixel(x, y).0);
                        if a[3] >= 128 && b[3] >= 128 {
                            sum += raster::oklab_delta(a, b) as f64;
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    total_delta += sum;
                    total_n += n;
                    cell_stats.push((sum / n as f64, [x0, y0, x1 - 1, y1 - 1]));
                }
            }
        }
        cell_stats.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let worst: Vec<Value> = cell_stats
            .iter()
            .take(5)
            .filter(|(d, _)| *d > 0.02)
            .map(|(d, r)| json!({"rect": r, "mean_delta": (d * 1000.0).round() / 1000.0}))
            .collect();
        // Reference colours the doc palette can't reach.
        let ref_pal = subject_palette(&small, 8);
        let doc_pal: Vec<[u8; 4]> = if doc.meta().palette.is_empty() {
            canvas
                .pixels()
                .filter(|p| p.0[3] > 0)
                .map(|p| p.0)
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect()
        } else {
            doc.meta().palette.clone()
        };
        let missing: Vec<Value> = ref_pal
            .iter()
            .filter(|rc| {
                doc_pal
                    .iter()
                    .all(|dc| raster::oklab_delta(**rc, *dc) > MISSING_TOL)
            })
            .map(|c| json!(c))
            .collect();
        // Inline visual: side-by-side (ref | canvas) or overlay (ref ghosted
        // under the canvas) at matched scale.
        let sc = preview_scale(cw.max(ch) * 2, ch);
        let img = match mode {
            CompareMode::Overlay => {
                let mut out = RgbaImage::from_pixel(cw, ch, Rgba([0, 0, 0, 0]));
                for (x, y, p) in small.enumerate_pixels() {
                    if p.0[3] > 0 {
                        out.put_pixel(x, y, Rgba([p.0[0], p.0[1], p.0[2], p.0[3] / 2]));
                    }
                }
                for (x, y, p) in canvas.enumerate_pixels() {
                    if p.0[3] > 0 {
                        out.put_pixel(x, y, *p);
                    }
                }
                out
            }
            CompareMode::SideBySide => {
                // side_by_side: reference | 2px gutter | canvas.
                let gut = 2;
                let mut out = RgbaImage::from_pixel(cw * 2 + gut, ch, Rgba([0, 0, 0, 0]));
                for (x, y, p) in small.enumerate_pixels() {
                    out.put_pixel(x, y, *p);
                }
                for y in 0..ch {
                    for g in 0..gut {
                        out.put_pixel(cw + g, y, Rgba([40, 40, 52, 255]));
                    }
                }
                for (x, y, p) in canvas.enumerate_pixels() {
                    out.put_pixel(cw + gut + x, y, *p);
                }
                out
            }
        };
        let scaled = super::scale_nn(&img, sc)?;
        let mean_delta = if total_n == 0 {
            Value::Null
        } else {
            json!((total_delta / total_n as f64 * 1000.0).round() / 1000.0)
        };
        Ok((
            encode_png(&scaled)?,
            json!({
                "silhouette_iou": (iou * 1000.0).round() / 1000.0,
                "mean_delta": mean_delta,
                "worst_cells": worst,
                "missing_reference_colors": missing,
                "mode": mode,
                "guide": "iou ≥ 0.80 = silhouette reads right; mean_delta ≤ 0.06 = colours read right. Fix worst_cells first.",
            }),
        ))
    }

    /// Per-PIXEL signed error map vs the reference (the see-and-repair eye the
    /// scalar `ref_compare` can't be). For every pixel where both canvas and
    /// reference are opaque it computes the OKLCh deltas — ΔL (value), ΔC
    /// (chroma), ΔH (hue, degrees) — and returns: a HEAT png (red = canvas too
    /// light, blue = too dark, green = wrong colour; brightness = ΔE) plus the
    /// `top` worst INDIVIDUAL pixels, each with an actionable fix direction. This
    /// converts the loop from "a number moved" to "this exact pixel is too dark —
    /// lighten it", so the agent can converge the last 5%.
    pub fn diff_map(&self, id: &str, frame: usize, top: usize) -> Result<(Vec<u8>, Value), String> {
        let (canvas, small, _doc) = self.ref_vs_canvas(id, frame)?;
        let (cw, ch) = (canvas.width(), canvas.height());
        let mut heat = RgbaImage::from_pixel(cw, ch, Rgba([0, 0, 0, 0]));
        // (ΔE, x, y, ΔL, ΔC, chroma-weighted ΔH magnitude)
        let mut worst: Vec<(f32, u32, u32, f32, f32, f32)> = Vec::new();
        let (mut sum, mut n, mut maxd) = (0f64, 0u64, 0f32);
        let (mut inter, mut union) = (0u64, 0u64);
        // One dominance rule drives BOTH the heat colour and the fix string, so
        // they can never contradict; hue is chroma-weighted so it vanishes on
        // near-gray pixels (where an OKLCh hue angle is meaningless).
        let classify = |dl: f32, dc: f32, hue: f32| -> bool {
            let value_err = dl.abs();
            let colour_err = dc.abs() + hue;
            value_err >= colour_err
        };
        for y in 0..ch {
            for x in 0..cw {
                let a = canvas.get_pixel(x, y).0;
                let b = small.get_pixel(x, y).0;
                let (ao, bo) = (a[3] >= 128, b[3] >= 128);
                if ao && bo {
                    inter += 1;
                }
                if ao || bo {
                    union += 1;
                }
                if !(ao && bo) {
                    continue;
                }
                let (la, ca, ha) = raster::oklab_to_oklch(raster::srgb_to_oklab(a));
                let (lb, cb, hb) = raster::oklab_to_oklch(raster::srgb_to_oklab(b));
                let (dl, dc) = (la - lb, ca - cb);
                let mut dd = ha - hb; // degrees, wrap to [-180, 180]
                if dd > 180.0 {
                    dd -= 360.0;
                } else if dd < -180.0 {
                    dd += 360.0;
                }
                // Chroma-weighted hue arc in OKLab units (comparable to ΔL/ΔC);
                // ~0 when either colour is achromatic.
                let hue = 2.0 * ca.min(cb) * (dd.to_radians() / 2.0).sin().abs();
                let de = raster::oklab_delta(a, b);
                sum += de as f64;
                n += 1;
                maxd = maxd.max(de);
                let value_dom = classify(dl, dc, hue);
                let i = ((de / 0.2).clamp(0.0, 1.0) * 255.0) as u8;
                let px = if value_dom {
                    if dl > 0.0 {
                        [i, 0, 0, 255] // too light
                    } else {
                        [0, 0, i, 255] // too dark
                    }
                } else {
                    [0, i, 0, 255] // wrong colour (chroma/hue)
                };
                heat.put_pixel(x, y, Rgba(px));
                if de > 0.02 {
                    worst.push((de, x, y, dl, dc, hue));
                }
            }
        }
        let iou = if union == 0 {
            0.0
        } else {
            inter as f64 / union as f64
        };
        worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let top = top.clamp(1, 64);
        let worst_pixels: Vec<Value> = worst
            .iter()
            .take(top)
            .map(|&(de, x, y, dl, dc, hue)| {
                let value_dom = classify(dl, dc, hue);
                let mut fix: Vec<&str> = Vec::new();
                if value_dom {
                    if dl.abs() > 0.02 {
                        fix.push(if dl > 0.0 { "darken" } else { "lighten" });
                    }
                } else {
                    if dc.abs() > 0.02 {
                        fix.push(if dc > 0.0 { "desaturate" } else { "saturate" });
                    }
                    if hue > 0.02 {
                        fix.push("shift hue toward reference");
                    }
                }
                if fix.is_empty() && dl.abs() > 0.02 {
                    fix.push(if dl > 0.0 { "darken" } else { "lighten" });
                }
                json!({
                    "x": x, "y": y,
                    "delta": (de * 1000.0).round() / 1000.0,
                    "fix": if fix.is_empty() { "minor".into() } else { fix.join(" + ") },
                })
            })
            .collect();
        let sc = preview_scale(cw, ch);
        let scaled = super::scale_nn(&heat, sc)?;
        let mean = if n == 0 {
            Value::Null
        } else {
            json!((sum / n as f64 * 1000.0).round() / 1000.0)
        };
        Ok((
            encode_png(&scaled)?,
            json!({
                "doc_id": id, "frame": frame,
                "mean_delta": mean,
                "max_delta": (maxd * 1000.0).round() / 1000.0,
                "compared_pixels": n,
                "silhouette_iou": (iou * 1000.0).round() / 1000.0,
                "worst_pixels": worst_pixels,
                "canvas_size": [cw, ch],
                "scale": sc,
                "heat_legend": "PNG is canvas×scale; worst_pixels x,y are doc coords. brightness = ΔE; red = too light, blue = too dark, green = wrong colour",
                "guide": "worst_pixels are only meaningful once silhouette_iou is high (≥0.8) — align the shape first, then fix the named pixels and re-run. delta ≤ 0.06 reads right.",
            }),
        ))
    }
}

/// Resolve a stored reference filename inside the doc dir, rejecting anything
/// that isn't a bare file name — the server only ever writes "reference.png",
/// so a doc.json edited to hold "../../<something>" must not turn the clear
/// path into an arbitrary file deletion (or the read path into a file probe).
fn ref_path(dir: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    let p = Path::new(name);
    let is_bare = p.components().count() == 1 && p.file_name().is_some() && !p.is_absolute();
    if !is_bare {
        return Err(format!(
            "stored reference name '{}' is not a bare filename — refusing",
            name
        ));
    }
    Ok(dir.join(p))
}

/// Downscale (area-average) so the longest side fits under `max`; smaller
/// Frequency-weighted subject palette: count the (half-)opaque colours and
/// median-cut them, so the subject owns every slot. Empty subject = empty palette.
fn subject_palette(img: &RgbaImage, colors: usize) -> Vec<[u8; 4]> {
    let mut counts: std::collections::HashMap<[u8; 3], u64> = std::collections::HashMap::new();
    for p in img.pixels().filter(|p| p.0[3] >= 128) {
        *counts.entry([p.0[0], p.0[1], p.0[2]]).or_insert(0) += 1;
    }
    let pairs: Vec<([u8; 3], u64)> = counts.into_iter().collect();
    if pairs.is_empty() {
        Vec::new()
    } else {
        raster::median_cut_weighted(&pairs, colors, &[])
    }
}

/// images pass through untouched.
fn fit_under(img: &RgbaImage, max: u32) -> RgbaImage {
    let longest = img.width().max(img.height());
    if longest <= max {
        return img.clone();
    }
    let tw = (img.width() as u64 * max as u64 / longest as u64).max(1) as u32;
    let th = (img.height() as u64 * max as u64 / longest as u64).max(1) as u32;
    raster::scale(img, tw, th, raster::ScaleMethod::AreaAverage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-ref-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    /// Paint the reference's 8x8 block footprint (x 4..=11) in `color`.
    fn block(s: &Studio, id: &str, color: [u8; 4]) {
        s.doc_draw(
            id,
            0,
            0,
            "rect",
            json!({"x0": 4, "y0": 0, "x1": 11, "y1": 7, "color": color, "fill": true})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    }

    /// 16x8 source: flat grey backdrop with a red 8x8 block in the middle.
    fn sample_png(tag: &str) -> std::path::PathBuf {
        let mut src = RgbaImage::from_pixel(16, 8, Rgba([90, 90, 90, 255]));
        for y in 0..8 {
            for x in 4..12 {
                src.put_pixel(x, y, Rgba([200, 30, 30, 255]));
            }
        }
        let p = std::env::temp_dir().join(format!("atelier-ref-src-{tag}.png"));
        src.save(&p).unwrap();
        p
    }

    #[test]
    fn set_reference_persists_and_clears() {
        let s = studio("set");
        let created = s.doc_new("d", 16, 8).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let p = sample_png("set");
        let r = s.set_reference(id, p.to_str()).unwrap();
        assert_eq!(r["source_size"], json!([16, 8]));
        // Stored with the doc: a fresh open still sees it.
        let info = s.doc_info(id).unwrap();
        assert_eq!(info["reference"], json!("reference.png"));
        // Analyze without a path uses the stored reference.
        let (png, rep) = s.ref_analyze(id, None, None, 4).unwrap();
        assert!(!png.is_empty());
        assert!(rep["bg_coverage_pct"].as_f64().unwrap() > 10.0);
        assert!(!rep["subject_palette"].as_array().unwrap().is_empty());
        assert!(
            s.ref_analyze(id, None, None, MAX_SUBJECT_PALETTE_COLORS + 1)
                .is_err()
        );
        // Clearing removes it.
        s.set_reference(id, None).unwrap();
        assert!(s.ref_analyze(id, None, None, 4).is_err());
    }

    #[test]
    fn ref_compare_scores_likeness() {
        let s = studio("cmp");
        let created = s.doc_new("d", 16, 8).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let p = sample_png("cmp");
        s.set_reference(id, p.to_str()).unwrap();
        // Faithful recreation: the same red block, backdrop left transparent.
        block(&s, id, [200, 30, 30, 255]);
        let (png, good) = s.ref_compare(id, 0, CompareMode::SideBySide, 4).unwrap();
        assert!(!png.is_empty());
        let iou = good["silhouette_iou"].as_f64().unwrap();
        assert!(iou > 0.9, "faithful copy should score high, got {iou}");
        // A wrong-colour copy keeps the silhouette but raises the colour delta.
        block(&s, id, [30, 30, 200, 255]);
        let (_png, bad) = s.ref_compare(id, 0, CompareMode::Overlay, 4).unwrap();
        assert!(bad["mean_delta"].as_f64().unwrap() > 0.1);
        assert!(
            !bad["missing_reference_colors"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn diff_map_flags_too_dark_pixels_to_lighten() {
        let s = studio("diff");
        let created = s.doc_new("d", 16, 8).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let p = sample_png("diff");
        s.set_reference(id, p.to_str()).unwrap();
        // Same silhouette as the reference's red block, but darker -> "lighten".
        block(&s, id, [120, 18, 18, 255]);
        let (png, r) = s.diff_map(id, 0, 10).unwrap();
        assert!(!png.is_empty());
        assert!(
            r["mean_delta"].as_f64().unwrap() > 0.05,
            "{}",
            r["mean_delta"]
        );
        let worst = r["worst_pixels"].as_array().unwrap();
        assert!(!worst.is_empty(), "expected worst pixels");
        assert!(
            worst[0]["fix"].as_str().unwrap().contains("lighten"),
            "darker canvas should say lighten: {:?}",
            worst[0]
        );
    }
}
