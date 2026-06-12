//! Reference-image subsystem: store the original a document is recreating,
//! view/analyze it through atelier's eye, and SCORE the canvas against it —
//! the closed loop that turns "does my sprite match the character?" from
//! cross-turn memory recall into a measurable, same-turn signal.

use std::path::Path;

use image::{Rgba, RgbaImage};
use serde_json::{json, Value};

use super::{encode_png, preview_scale, Studio};
use crate::document::Document;
use crate::raster;

/// OKLab ΔE used for corner-flood background detection on references.
const BG_TOL: f32 = 0.08;
/// A reference colour with no doc colour within this ΔE counts as missing.
const MISSING_TOL: f32 = 0.12;

impl Studio {
    /// Attach a reference image to a document: the file is decoded (validation),
    /// re-encoded as PNG into the doc dir, and recorded in the meta so it
    /// persists with the document. `path` None clears the reference.
    pub fn set_reference(&self, id: &str, path: Option<&str>) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let Some(path) = path else {
            if let Some(name) = doc.meta.reference.take() {
                if let Ok(p) = ref_path(&dir, &name) {
                    let _ = std::fs::remove_file(p);
                }
            }
            doc.save(&dir)?;
            return Ok(json!({"ok": true, "doc_id": id, "reference": Value::Null}));
        };
        let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
        let (rw, rh) = (img.width(), img.height());
        img.save(dir.join("reference.png"))
            .map_err(|e| e.to_string())?;
        doc.meta.reference = Some("reference.png".into());
        doc.save(&dir)?;
        let (cw, ch) = (doc.meta.w, doc.meta.h);
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
            .meta
            .reference
            .as_ref()
            .ok_or("document has no reference image — doc_set_reference first")?;
        Ok(image::open(ref_path(dir, name)?)
            .map_err(|e| format!("reference unreadable: {e}"))?
            .to_rgba8())
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
        let (dir, doc) = self.open(id)?;
        let src = match path {
            Some(p) => image::open(p).map_err(|e| e.to_string())?.to_rgba8(),
            None => Self::ref_image(&dir, &doc)?,
        };
        let (rw, rh) = (src.width(), src.height());
        // Subject = source minus corner-flooded backdrop (non-destructive copy).
        let mut subject = src.clone();
        raster::remove_background(&mut subject, BG_TOL);
        let total = (rw * rh) as u64;
        let cleared = subject.pixels().filter(|p| p.0[3] == 0).count() as u64
            - src.pixels().filter(|p| p.0[3] == 0).count() as u64;
        let tw = target_w.unwrap_or(doc.meta.w).max(1);
        let th = ((rh as f64 * tw as f64 / rw.max(1) as f64).round() as u32).max(1);
        // Same 1M-pixel cap as import_clean — an oversized target_w would
        // otherwise allocate an unbounded image in one call.
        if tw as usize * th as usize > 1_048_576 {
            return Err(format!(
                "target {}x{} is over the 1M-pixel cap — pass a smaller target_w",
                tw, th
            ));
        }
        let small = raster::downscale_area(&subject, tw, th);
        // Frequency-weighted subject palette of the downscaled art.
        let mut counts: std::collections::HashMap<[u8; 3], u64> = std::collections::HashMap::new();
        for p in small.pixels().filter(|p| p.0[3] >= 128) {
            *counts.entry([p.0[0], p.0[1], p.0[2]]).or_insert(0) += 1;
        }
        let pairs: Vec<([u8; 3], u64)> = counts.into_iter().collect();
        let palette = if pairs.is_empty() {
            Vec::new()
        } else {
            raster::median_cut_weighted(&pairs, colors.max(2), &[])
        };
        let pal_json: Vec<Value> = palette.iter().map(|c| json!(c)).collect();
        // Silhouette grid at target size (capped so the text stays readable).
        let silhouette: Value = if (tw * th) <= 4096 {
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
        mode: &str,
        cells: u32,
    ) -> Result<(Vec<u8>, Value), String> {
        let (dir, doc) = self.open(id)?;
        let src = Self::ref_image(&dir, &doc)?;
        let canvas = doc.analysis_image(None, frame)?;
        let (cw, ch) = (canvas.width(), canvas.height());
        let mut subject = src.clone();
        raster::remove_background(&mut subject, BG_TOL);
        let small = raster::downscale_area(&subject, cw, ch);
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
        let mut counts: std::collections::HashMap<[u8; 3], u64> = std::collections::HashMap::new();
        for p in small.pixels().filter(|p| p.0[3] >= 128) {
            *counts.entry([p.0[0], p.0[1], p.0[2]]).or_insert(0) += 1;
        }
        let pairs: Vec<([u8; 3], u64)> = counts.into_iter().collect();
        let ref_pal = if pairs.is_empty() {
            Vec::new()
        } else {
            raster::median_cut_weighted(&pairs, 8, &[])
        };
        let doc_pal: Vec<[u8; 4]> = if doc.meta.palette.is_empty() {
            canvas
                .pixels()
                .filter(|p| p.0[3] > 0)
                .map(|p| p.0)
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect()
        } else {
            doc.meta.palette.clone()
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
            "overlay" => {
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
            _ => {
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
        let scaled = if sc > 1 {
            image::imageops::resize(
                &img,
                img.width() * sc,
                img.height() * sc,
                image::imageops::FilterType::Nearest,
            )
        } else {
            img
        };
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
                "mode": if mode == "overlay" { "overlay" } else { "side_by_side" },
                "guide": "iou ≥ 0.80 = silhouette reads right; mean_delta ≤ 0.06 = colours read right. Fix worst_cells first.",
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
/// images pass through untouched.
fn fit_under(img: &RgbaImage, max: u32) -> RgbaImage {
    let longest = img.width().max(img.height());
    if longest <= max {
        return img.clone();
    }
    let tw = (img.width() as u64 * max as u64 / longest as u64).max(1) as u32;
    let th = (img.height() as u64 * max as u64 / longest as u64).max(1) as u32;
    raster::downscale_area(img, tw, th)
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
        s.doc_create("d", 16, 8).unwrap();
        let p = sample_png("set");
        let r = s.set_reference("d", p.to_str()).unwrap();
        assert_eq!(r["source_size"], json!([16, 8]));
        // Stored with the doc: a fresh open still sees it.
        let info = s.doc_info("d").unwrap();
        assert_eq!(info["reference"], json!("reference.png"));
        // Analyze without a path uses the stored reference.
        let (png, rep) = s.ref_analyze("d", None, None, 4).unwrap();
        assert!(!png.is_empty());
        assert!(rep["bg_coverage_pct"].as_f64().unwrap() > 10.0);
        assert!(!rep["subject_palette"].as_array().unwrap().is_empty());
        // Clearing removes it.
        s.set_reference("d", None).unwrap();
        assert!(s.ref_analyze("d", None, None, 4).is_err());
    }

    #[test]
    fn ref_compare_scores_likeness() {
        let s = studio("cmp");
        s.doc_create("d", 16, 8).unwrap();
        let p = sample_png("cmp");
        s.set_reference("d", p.to_str()).unwrap();
        // Faithful recreation: the same red block, backdrop left transparent.
        s.doc_rect("d", 0, 0, 4, 0, 11, 7, [200, 30, 30, 255], true, 1)
            .unwrap();
        let (png, good) = s.ref_compare("d", 0, "side_by_side", 4).unwrap();
        assert!(!png.is_empty());
        let iou = good["silhouette_iou"].as_f64().unwrap();
        assert!(iou > 0.9, "faithful copy should score high, got {iou}");
        // A wrong-colour copy keeps the silhouette but raises the colour delta.
        s.doc_rect("d", 0, 0, 4, 0, 11, 7, [30, 30, 200, 255], true, 1)
            .unwrap();
        let (_png, bad) = s.ref_compare("d", 0, "overlay", 4).unwrap();
        assert!(bad["mean_delta"].as_f64().unwrap() > 0.1);
        assert!(!bad["missing_reference_colors"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}
