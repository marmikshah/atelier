//! Read-only canvas analysis — the agent's other eye. These readers never
//! mutate the document (and ignore any active selection); they describe what is
//! already on the canvas: a text-grid dump, a silhouette report, connected
//! components, and a coarse coverage heatmap.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use super::Studio;

/// Shared >4096-px area cap for the grid-emitting readers. Builds the error from
/// a `label` ("region" / "diff region") and tail `advice`; `doc_dump_region` and
/// `doc_frame_diff` both gate on it so their grids stay readable.
fn area_cap_check(label: &str, w: u64, h: u64, advice: &str) -> Result<(), String> {
    let area = w * h;
    if area > crate::GRID_AREA_CAP {
        return Err(format!(
            "{} is {}x{}={} px (>4096) — {}",
            label, w, h, area, advice
        ));
    }
    Ok(())
}

impl Studio {
    // -- canvas readers (read-only analysis; ignore any active selection) ----

    /// Text-grid dump of pixels for blind inspection. `layer` None → flattened
    /// composite, else that cel. `region` (inclusive corners) defaults to the
    /// whole canvas. `mode` "symbol" assigns A..Z a..z 0..9 per distinct colour
    /// (`.` = transparent); "hex" emits space-separated #rrggbb(aa)/`.` tokens.
    pub fn doc_dump_region(
        &self,
        id: &str,
        frame: usize,
        layer: Option<usize>,
        region: Option<(i32, i32, i32, i32)>,
        mode: &str,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let img = doc.analysis_image(layer, frame)?;
        let (cw, ch) = (doc.meta.w as i32, doc.meta.h as i32);
        let (x0, y0, x1, y1) = atelier_core::raster::resolve_region(region, cw as u32, ch as u32)?;
        let (w, h) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
        area_cap_check(
            "region",
            w as u64,
            h as u64,
            "crop with a smaller `region` first",
        )?;
        let px = |x: i32, y: i32| img.get_pixel(x as u32, y as u32).0;
        if mode == "hex" {
            let rows: Vec<String> = (y0..=y1)
                .map(|y| {
                    (x0..=x1)
                        .map(|x| {
                            let p = px(x, y);
                            if p[3] == 0 {
                                ".".to_string()
                            } else if p[3] == 255 {
                                crate::hex_rgb(&p)
                            } else {
                                crate::hex_rgba(&p)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            return Ok(json!({"w": w, "h": h, "origin": [x0, y0], "mode": "hex", "rows": rows}));
        }
        // symbol mode: first-seen colour → glyph, transparent → '.'
        const GLYPHS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut order: Vec<[u8; 4]> = Vec::new();
        let mut rows: Vec<String> = Vec::with_capacity(h as usize);
        for y in y0..=y1 {
            let mut row = String::with_capacity(w as usize);
            for x in x0..=x1 {
                let p = px(x, y);
                if p[3] == 0 {
                    row.push('.');
                    continue;
                }
                let gi = match order.iter().position(|c| *c == p) {
                    Some(i) => i,
                    None => {
                        order.push(p);
                        order.len() - 1
                    }
                };
                if gi >= GLYPHS.len() {
                    return Err(format!(
                        "{} distinct colours exceed the 62 symbol glyphs — use mode \"hex\"",
                        order.len()
                    ));
                }
                row.push(GLYPHS[gi] as char);
            }
            rows.push(row);
        }
        let legend: serde_json::Map<String, Value> = order
            .iter()
            .enumerate()
            .map(|(i, c)| ((GLYPHS[i] as char).to_string(), json!(crate::hex_rgba(c))))
            .collect();
        Ok(
            json!({"w": w, "h": h, "origin": [x0, y0], "mode": "symbol", "legend": legend, "rows": rows}),
        )
    }

    /// Opaque-vs-transparent shape report: tight bbox, opaque/canvas fill ratio,
    /// and a `#`/`.` grid of the whole canvas. `alpha_threshold` is the minimum
    /// alpha counted as opaque.
    pub fn doc_silhouette(
        &self,
        id: &str,
        frame: usize,
        layer: Option<usize>,
        alpha_threshold: u8,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let img = doc.analysis_image(layer, frame)?;
        let (w, h) = (img.width(), img.height());
        let mut bbox: Option<[i32; 4]> = None;
        let mut opaque = 0u64;
        for (x, y, p) in img.enumerate_pixels() {
            if p.0[3] >= alpha_threshold {
                opaque += 1;
                let (xi, yi) = (x as i32, y as i32);
                bbox = Some(match bbox {
                    Some([a, b, c, d]) => [a.min(xi), b.min(yi), c.max(xi), d.max(yi)],
                    None => [xi, yi, xi, yi],
                });
            }
        }
        // The text grid is capped like doc_dump_region's — an uncapped
        // 128x128 grid is ~4K tokens of mostly dots.
        let grid: Value = if (w as u64) * (h as u64) <= crate::GRID_AREA_CAP {
            let rows: Vec<String> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| {
                            if img.get_pixel(x, y).0[3] >= alpha_threshold {
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
                "skipped — {}x{} exceeds the 4096-px grid cap; use doc_look or doc_dump_region on a region",
                w, h
            ))
        };
        let fill_ratio = opaque as f64 / (w as u64 * h as u64) as f64;
        Ok(json!({
            "bbox": bbox.map(|b| json!(b)).unwrap_or(Value::Null),
            "fill_ratio": (fill_ratio * 1000.0).round() / 1000.0,
            "grid": grid,
        }))
    }

    /// Connected-component report over opaque (or exact-`color`) pixels.
    /// `connectivity` 4|8; `min_area` filters the listed components (specks =
    /// area ≤ 2 are always reported separately). Components sorted by area desc,
    /// list capped at 64 (sets `truncated`).
    pub fn doc_components(
        &self,
        id: &str,
        frame: usize,
        layer: Option<usize>,
        connectivity: u8,
        color: Option<[u8; 4]>,
        min_area: u32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let img = doc.analysis_image(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        // membership: exact colour match if given, else any opaque pixel.
        let member = |x: i32, y: i32| -> bool {
            let p = img.get_pixel(x as u32, y as u32).0;
            match color {
                Some(c) => p == c,
                None => p[3] > 0,
            }
        };
        let neigh: &[(i32, i32)] = if connectivity == 4 {
            &[(-1, 0), (1, 0), (0, -1), (0, 1)]
        } else {
            &[
                (-1, 0),
                (1, 0),
                (0, -1),
                (0, 1),
                (-1, -1),
                (1, -1),
                (-1, 1),
                (1, 1),
            ]
        };
        let mut seen = vec![false; (w * h) as usize];
        struct Comp {
            bbox: [i32; 4], // tight [x0,y0,x1,y1]
            area: u32,      // opaque pixel count
            sx: u64,        // Σx (for the centroid)
            sy: u64,        // Σy (for the centroid)
            // colour → count for the dominant readout; a small Vec beats a
            // HashMap per component (specks allocate nothing).
            colors: Vec<([u8; 4], u32)>,
        }
        let mut comps: Vec<Comp> = Vec::new();
        for sy in 0..h {
            for sx in 0..w {
                let si = (sy * w + sx) as usize;
                if seen[si] || !member(sx, sy) {
                    continue;
                }
                // DFS the component (Vec-as-stack).
                let mut stack = vec![(sx, sy)];
                seen[si] = true;
                let mut c = Comp {
                    bbox: [sx, sy, sx, sy],
                    area: 0,
                    sx: 0,
                    sy: 0,
                    colors: Vec::new(),
                };
                while let Some((x, y)) = stack.pop() {
                    c.area += 1;
                    c.sx += x as u64;
                    c.sy += y as u64;
                    c.bbox[0] = c.bbox[0].min(x);
                    c.bbox[1] = c.bbox[1].min(y);
                    c.bbox[2] = c.bbox[2].max(x);
                    c.bbox[3] = c.bbox[3].max(y);
                    let p = img.get_pixel(x as u32, y as u32).0;
                    match c.colors.iter_mut().find(|(col, _)| *col == p) {
                        Some((_, n)) => *n += 1,
                        None => c.colors.push((p, 1)),
                    }
                    for (ox, oy) in neigh {
                        let (nx, ny) = (x + ox, y + oy);
                        if nx < 0 || ny < 0 || nx >= w || ny >= h {
                            continue;
                        }
                        let ni = (ny * w + nx) as usize;
                        if !seen[ni] && member(nx, ny) {
                            seen[ni] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
                comps.push(c);
            }
        }
        let specks: Vec<Value> = comps
            .iter()
            .filter(|c| c.area <= 2)
            .map(|c| json!([c.bbox[0], c.bbox[1]]))
            .collect();
        let mut listed: Vec<&Comp> = comps.iter().filter(|c| c.area >= min_area.max(1)).collect();
        listed.sort_by_key(|c| std::cmp::Reverse(c.area));
        let truncated = listed.len() > 64;
        listed.truncate(64);
        let components: Vec<Value> = listed
            .iter()
            .map(|c| {
                let dom = c
                    .colors
                    .iter()
                    .max_by_key(|(_, n)| *n)
                    .map(|(c, _)| *c)
                    .unwrap_or([0, 0, 0, 0]);
                json!({
                    "bbox": c.bbox,
                    "centroid": [
                        (c.sx / c.area as u64) as i32,
                        (c.sy / c.area as u64) as i32,
                    ],
                    "area": c.area,
                    "dominant": crate::hex_rgb(&dom),
                })
            })
            .collect();
        Ok(json!({
            "count": components.len(),
            "components": components,
            "specks": specks,
            "truncated": truncated,
        }))
    }

    /// Per-form shading audit — the eye for the #1 beginner failure the scalar
    /// suite could not see. For each connected opaque component (a "form") it
    /// infers the light direction from a least-squares fit of perceptual
    /// lightness across the form, and flags **pillow-shading** (brightness that
    /// hugs the silhouette centre instead of a light direction) plus whether the
    /// forms agree on one light. `min_area` skips specks (default 12).
    pub fn doc_form_audit(
        &self,
        id: &str,
        frame: usize,
        layer: Option<usize>,
        min_area: u32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let img = doc.analysis_image(layer, frame)?;
        Ok(form_audit_image(&img, min_area))
    }

    /// Coarse coverage heatmap: split the canvas into `rows`×`cols` cells, each
    /// reporting opaque fill 0..1 and mean luma (null if the cell is empty), plus
    /// the content bbox and its centre offset from the canvas centre.
    pub fn doc_coverage_map(
        &self,
        id: &str,
        frame: usize,
        cols: u32,
        rows: u32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let img = doc.analysis_image(None, frame)?;
        let (w, h) = (img.width(), img.height());
        let (cols, rows) = (cols.max(1), rows.max(1));
        let mut bbox: Option<[i32; 4]> = None;
        let mut grid: Vec<Vec<Value>> = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            // Cell pixel bounds (evenly split, last cell absorbs the remainder).
            let cy0 = (r as u64 * h as u64 / rows as u64) as u32;
            let cy1 = ((r + 1) as u64 * h as u64 / rows as u64) as u32;
            let mut row_cells: Vec<Value> = Vec::with_capacity(cols as usize);
            for c in 0..cols {
                let cx0 = (c as u64 * w as u64 / cols as u64) as u32;
                let cx1 = ((c + 1) as u64 * w as u64 / cols as u64) as u32;
                let (mut total, mut opaque, mut luma_sum) = (0u64, 0u64, 0u64);
                for y in cy0..cy1 {
                    for x in cx0..cx1 {
                        total += 1;
                        let p = img.get_pixel(x, y).0;
                        if p[3] > 0 {
                            opaque += 1;
                            luma_sum += atelier_core::raster::luma(p) as u64;
                            let (xi, yi) = (x as i32, y as i32);
                            bbox = Some(match bbox {
                                Some([a, b, cc, d]) => {
                                    [a.min(xi), b.min(yi), cc.max(xi), d.max(yi)]
                                }
                                None => [xi, yi, xi, yi],
                            });
                        }
                    }
                }
                let fill = if total == 0 {
                    0.0
                } else {
                    (opaque as f64 / total as f64 * 1000.0).round() / 1000.0
                };
                let value = match luma_sum.checked_div(opaque) {
                    Some(mean) => json!(mean as u32),
                    None => Value::Null,
                };
                row_cells.push(json!({"fill": fill, "value": value}));
            }
            grid.push(row_cells);
        }
        let center_offset = match bbox {
            Some([a, b, c, d]) => {
                let (bcx, bcy) = ((a + c) as f64 / 2.0, (b + d) as f64 / 2.0);
                let (ccx, ccy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
                json!([
                    ((bcx - ccx) * 100.0).round() / 100.0,
                    ((bcy - ccy) * 100.0).round() / 100.0
                ])
            }
            None => json!([0, 0]),
        };
        Ok(json!({
            "grid": grid,
            "content_bbox": bbox.map(|b| json!(b)).unwrap_or(Value::Null),
            "center_offset": center_offset,
        }))
    }

    // -- value & colour feedback (read-only analysis) -----------------------

    /// WCAG contrast check in one of three modes. `region`: mean colour inside vs
    /// a 4px surrounding band. `palette`: every pair of the frame's distinct
    /// opaque colours (capped 16). `one-bit`: threshold luma to a pure B/W PNG and
    /// report black/white coverage. `pass` = ratio ≥ `min_ratio`.
    pub fn doc_contrast_check(
        &self,
        id: &str,
        frame: usize,
        mode: &str,
        region: Option<(i32, i32, i32, i32)>,
        min_ratio: f32,
        threshold: u8,
        out_path: Option<&str>,
    ) -> Result<(Option<Vec<u8>>, Value), String> {
        let (dir, doc) = self.open(id)?;
        let img = doc.analysis_image(None, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let round2 = |v: f32| ((v * 100.0).round() / 100.0) as f64;
        match mode {
            "region" => {
                let (rx0, ry0, rx1, ry1) =
                    region.ok_or("region mode needs `region` [x0,y0,x1,y1]")?;
                let (x0, y0, x1, y1) = atelier_core::raster::resolve_region(
                    Some((rx0, ry0, rx1, ry1)),
                    w as u32,
                    h as u32,
                )?;
                // Mean opaque colour inside the region.
                let mut inside = [0u64; 3];
                let mut n_in = 0u64;
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let p = img.get_pixel(x as u32, y as u32).0;
                        if p[3] > 0 {
                            for c in 0..3 {
                                inside[c] += p[c] as u64;
                            }
                            n_in += 1;
                        }
                    }
                }
                if n_in == 0 {
                    return Err("region has no opaque pixels to measure".into());
                }
                // Mean opaque colour of the 4px band surrounding the region.
                let (bx0, by0) = (x0 - 4, y0 - 4);
                let (bx1, by1) = (x1 + 4, y1 + 4);
                let mut band = [0u64; 3];
                let mut n_band = 0u64;
                for y in by0..=by1 {
                    for x in bx0..=bx1 {
                        if x < 0 || y < 0 || x >= w || y >= h {
                            continue;
                        }
                        // Skip the region interior — only its surrounding ring.
                        if x >= x0 && x <= x1 && y >= y0 && y <= y1 {
                            continue;
                        }
                        let p = img.get_pixel(x as u32, y as u32).0;
                        if p[3] > 0 {
                            for c in 0..3 {
                                band[c] += p[c] as u64;
                            }
                            n_band += 1;
                        }
                    }
                }
                if n_band == 0 {
                    return Err(
                        "the 4px band around the region has no opaque pixels to contrast against"
                            .into(),
                    );
                }
                let mean = |s: [u64; 3], n: u64| -> [u8; 4] {
                    [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8, 255]
                };
                let (a, b) = (mean(inside, n_in), mean(band, n_band));
                let ratio = atelier_core::raster::wcag_ratio(a, b);
                Ok((
                    None,
                    json!({
                        "mode": "region",
                        "inside": crate::hex_rgb(&a),
                        "surround": crate::hex_rgb(&b),
                        "ratio": round2(ratio),
                        "pass": ratio >= min_ratio,
                    }),
                ))
            }
            "palette" => {
                // Distinct opaque colours of the frame (first-seen order).
                let mut colors: Vec<[u8; 4]> = Vec::new();
                for p in img.pixels() {
                    if p.0[3] > 0 && !colors.contains(&p.0) {
                        colors.push(p.0);
                        if colors.len() > 16 {
                            return Err(format!(
                                "frame has >16 distinct opaque colours — run doc_quantize first, \
                                 then re-check ({} pairs would be too many to read)",
                                colors.len()
                            ));
                        }
                    }
                }
                let mut pairs: Vec<Value> = Vec::new();
                let mut failures: Vec<Value> = Vec::new();
                for i in 0..colors.len() {
                    for j in (i + 1)..colors.len() {
                        let ratio = atelier_core::raster::wcag_ratio(colors[i], colors[j]);
                        let pass = ratio >= min_ratio;
                        let hex = |c: [u8; 4]| crate::hex_rgb(&c);
                        let entry = json!({
                            "a": hex(colors[i]),
                            "b": hex(colors[j]),
                            "ratio": round2(ratio),
                            "pass": pass,
                        });
                        if !pass {
                            failures.push(entry.clone());
                        }
                        pairs.push(entry);
                    }
                }
                Ok((
                    None,
                    json!({
                        "mode": "palette",
                        "colors": colors.len(),
                        "pairs": pairs,
                        "failures": failures,
                    }),
                ))
            }
            "one-bit" => {
                use image::{Rgba, RgbaImage};
                let out = match out_path {
                    Some(p) => PathBuf::from(p),
                    None => dir.join(format!("onebit_f{}.png", frame)),
                };
                if let Some(p) = out.parent() {
                    fs::create_dir_all(p)
                        .map_err(|e| format!("cannot create {}: {e}", p.display()))?;
                }
                let mut bw = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
                let (mut black, mut white) = (0u64, 0u64);
                for (x, y, p) in img.enumerate_pixels() {
                    if p.0[3] == 0 {
                        continue;
                    }
                    if atelier_core::raster::luma(p.0) >= threshold {
                        bw.put_pixel(x, y, Rgba([255, 255, 255, 255]));
                        white += 1;
                    } else {
                        bw.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                        black += 1;
                    }
                }
                bw.save(&out).map_err(|e| e.to_string())?;
                // Inline the B/W render like frame_diff/seam_report do, so the
                // agent sees the notan without a second call.
                let sc = crate::preview_scale(bw.width(), bw.height());
                let png = crate::encode_png(&crate::scale_nn(&bw, sc))?;
                let total = (black + white).max(1) as f64;
                Ok((
                    Some(png),
                    json!({
                        "mode": "one-bit",
                        "path": out.to_string_lossy(),
                        "threshold": threshold,
                        "black_pct": (black as f64 / total * 1000.0).round() / 1000.0,
                        "white_pct": (white as f64 / total * 1000.0).round() / 1000.0,
                    }),
                ))
            }
            other => Err(format!(
                "unknown contrast mode '{}' — use region|palette|one-bit",
                other
            )),
        }
    }

    /// Colour histogram for a frame (or all frames): each distinct opaque colour
    /// with its pixel count, percent, and whether it is in the locked palette.
    /// Flags off-palette colours and near-duplicate pairs (channel distance ≤
    /// `dupe_threshold`). Sorted by pixel count desc, capped at 256.
    pub fn doc_palette_report(
        &self,
        id: &str,
        frame: Option<usize>,
        layer: Option<usize>,
        region: Option<(i32, i32, i32, i32)>,
        dupe_threshold: i32,
    ) -> Result<Value, String> {
        use std::collections::HashMap;
        let (_dir, doc) = self.open(id)?;
        let frames: Vec<usize> = match frame {
            Some(f) => vec![f],
            None => (0..doc.meta.frames.len()).collect(),
        };
        let (cw, ch) = (doc.meta.w as i32, doc.meta.h as i32);
        let mut counts: HashMap<[u8; 4], u64> = HashMap::new();
        let mut total = 0u64;
        let (x0, y0, x1, y1) = atelier_core::raster::resolve_region(region, cw as u32, ch as u32)?;
        for f in frames {
            let img = doc.analysis_image(layer, f)?;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let p = img.get_pixel(x as u32, y as u32).0;
                    if p[3] > 0 {
                        *counts.entry(p).or_insert(0) += 1;
                        total += 1;
                    }
                }
            }
        }
        // Sort by pixel count desc (ties broken by colour for stable output).
        let mut entries: Vec<([u8; 4], u64)> = counts.into_iter().collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let has_palette = !doc.meta.palette.is_empty();
        let in_pal = |c: [u8; 4]| -> bool { doc.meta.palette.contains(&c) };
        let hex = |c: [u8; 4]| crate::hex_rgba(&c);
        let count = entries.len();
        // Top-48 with an "others" rollup: an unquantized import has hundreds
        // of distinct colours, and 256 JSON objects of them helps nobody.
        /// The report lists at most this many colours; the tail is summarised.
        const PALETTE_LIST_CAP: usize = 48;
        let truncated = count > PALETTE_LIST_CAP;
        let listed: Vec<&([u8; 4], u64)> = entries.iter().take(PALETTE_LIST_CAP).collect();
        let others_pixels: u64 = entries.iter().skip(PALETTE_LIST_CAP).map(|(_, n)| n).sum();
        // Off-palette tally covers EVERY distinct colour, not just the listed top.
        let off_palette_count = if has_palette {
            entries.iter().filter(|(c, _)| !in_pal(*c)).count() as u32
        } else {
            0
        };
        let colors: Vec<Value> = listed
            .iter()
            .map(|(c, n)| {
                let inp = if has_palette {
                    json!(in_pal(*c))
                } else {
                    Value::Null
                };
                json!({
                    "hex": hex(*c),
                    "rgba": c,
                    "pixels": n,
                    "percent": if total == 0 {
                        0.0
                    } else {
                        (*n as f64 / total as f64 * 1000.0).round() / 1000.0
                    },
                    "in_palette": inp,
                })
            })
            .collect();
        // Near-dupes: pairs within `dupe_threshold` max-channel distance (listed set).
        let mut near_dupes: Vec<Value> = Vec::new();
        for i in 0..listed.len() {
            for j in (i + 1)..listed.len() {
                let (a, b) = (listed[i].0, listed[j].0);
                let dist = (0..4)
                    .map(|k| (a[k] as i32 - b[k] as i32).abs())
                    .max()
                    .unwrap();
                if dist <= dupe_threshold {
                    near_dupes.push(json!([hex(a), hex(b), dist]));
                }
            }
        }
        let mut out = json!({
            "count": count,
            "colors": colors,
            "off_palette_count": if has_palette { json!(off_palette_count) } else { Value::Null },
            "near_dupes": near_dupes,
            "truncated": truncated,
        });
        if truncated {
            out["others"] = json!({
                "colors": count - PALETTE_LIST_CAP,
                "pixels": others_pixels,
                "note": "rolled up — quantize/snap_palette first for a readable report",
            });
        }
        Ok(out)
    }

    /// Validate a colour ramp's craft: monotonic value, even value spacing, hue
    /// drift direction, and saturation arc — from explicit `colors` or a
    /// document's locked palette (optionally a `slice` of it). Doc-independent.
    pub fn doc_ramp_validate(
        &self,
        colors: Option<Vec<[u8; 4]>>,
        id: Option<&str>,
        slice: Option<(usize, usize)>,
    ) -> Result<Value, String> {
        let ramp: Vec<[u8; 4]> = match (colors, id) {
            (Some(c), _) => c,
            (None, Some(doc_id)) => {
                let (_dir, doc) = self.open(doc_id)?;
                let pal = doc.meta.palette.clone();
                if pal.is_empty() {
                    return Err(format!(
                        "document '{}' has no locked palette — set one with doc_set_palette \
                         or pass explicit `colors`",
                        doc_id
                    ));
                }
                match slice {
                    Some((s, e)) => {
                        let end = e.min(pal.len());
                        if s >= end {
                            return Err(format!(
                                "slice [{},{}] is empty for a {}-colour palette",
                                s,
                                e,
                                pal.len()
                            ));
                        }
                        pal[s..end].to_vec()
                    }
                    None => pal,
                }
            }
            (None, None) => {
                return Err("pass `colors` [[r,g,b],...] or a `doc_id` to validate".into())
            }
        };
        if ramp.len() < 2 {
            return Err("a ramp needs at least 2 colours".into());
        }
        let lumas: Vec<i32> = ramp
            .iter()
            .map(|c| atelier_core::raster::luma(*c) as i32)
            .collect();
        let value_deltas: Vec<i32> = lumas.windows(2).map(|w| w[1] - w[0]).collect();
        // Monotonic value: every step moves the same direction (allow flats).
        let any_up = value_deltas.iter().any(|d| *d > 0);
        let any_down = value_deltas.iter().any(|d| *d < 0);
        let monotonic_value = !(any_up && any_down);
        // Even spacing: max |delta| deviation from the mean ≤ 25% of the mean.
        let mean_abs = value_deltas
            .iter()
            .map(|d| d.unsigned_abs() as f64)
            .sum::<f64>()
            / value_deltas.len() as f64;
        let even_spacing = if mean_abs < f64::EPSILON {
            true
        } else {
            value_deltas
                .iter()
                .all(|d| (d.unsigned_abs() as f64 - mean_abs).abs() <= 0.25 * mean_abs)
        };
        // Hue shift per step: signed shortest-arc in degrees.
        let hues: Vec<f32> = ramp
            .iter()
            .map(|c| atelier_core::raster::hue_deg(*c))
            .collect();
        let arc = |a: f32, b: f32| -> f32 {
            let mut d = b - a;
            while d > 180.0 {
                d -= 360.0;
            }
            while d < -180.0 {
                d += 360.0;
            }
            d
        };
        let hue_shift_deg: Vec<f64> = hues
            .windows(2)
            .map(|w| (arc(w[0], w[1]) as f64 * 10.0).round() / 10.0)
            .collect();
        // Direction: as value climbs, does hue go warm (→~50°) or cool (→~250°)?
        // Read drift sign on the steps with meaningful hue movement.
        let net: f64 = hue_shift_deg.iter().filter(|d| d.abs() >= 2.0).sum();
        let hue_direction = if net.abs() < 2.0 {
            "none"
        } else {
            let pos = hue_shift_deg.iter().filter(|d| **d >= 2.0).count();
            let neg = hue_shift_deg.iter().filter(|d| **d <= -2.0).count();
            if pos > 0 && neg > 0 {
                "mixed"
            } else if net > 0.0 {
                "cool-to-warm"
            } else {
                "warm-to-cool"
            }
        };
        let sat: Vec<f32> = ramp
            .iter()
            .map(|c| atelier_core::raster::saturation(*c))
            .collect();
        let sat_arc: Vec<f64> = sat
            .windows(2)
            .map(|w| ((w[1] - w[0]) as f64 * 1000.0).round() / 1000.0)
            .collect();
        // Warnings: call out value reversals (the readability killer in a ramp).
        let mut warnings: Vec<String> = Vec::new();
        for (i, d) in value_deltas.iter().enumerate() {
            if (any_up && *d < 0) || (any_down && *d > 0) {
                warnings.push(format!(
                    "step {}→{} value reverses ({} -> {})",
                    i,
                    i + 1,
                    lumas[i],
                    lumas[i + 1]
                ));
            }
        }
        if !even_spacing {
            warnings.push("uneven value spacing (a step deviates >25% from the mean)".into());
        }
        Ok(json!({
            "count": ramp.len(),
            "monotonic_value": monotonic_value,
            "value_deltas": value_deltas,
            "even_spacing": even_spacing,
            "hue_shift_deg": hue_shift_deg,
            "hue_direction": hue_direction,
            "sat_arc": sat_arc,
            "warnings": warnings,
        }))
    }

    // -- animation & tiling feedback (read-only) ----------------------------

    /// Diff two frames pixel-by-pixel. `layer` None flattens. `region` restricts
    /// the compared area (else whole canvas). `grid` adds a text map (`.`unchanged
    /// `+`added `-`removed `~`recolored, area-capped like doc_dump_region).
    /// `render` "overlay" produces a PNG with frame_b dimmed 40% and changed
    /// pixels flagged (green=added, red=removed, yellow=recoloured) — returned as
    /// bytes for the MCP layer to inline, and written to `out_path` when given.
    /// Returns the change tallies and the bbox of all changed pixels.
    pub fn doc_frame_diff(
        &self,
        id: &str,
        frame_a: usize,
        frame_b: usize,
        layer: Option<usize>,
        region: Option<(i32, i32, i32, i32)>,
        grid: bool,
        render: &str,
        out_path: Option<&str>,
        scale: u32,
    ) -> Result<(Option<Vec<u8>>, Value), String> {
        use image::{Rgba, RgbaImage};
        let (dir, doc) = self.open(id)?;
        let (cw, ch) = (doc.meta.w as i32, doc.meta.h as i32);
        let (x0, y0, x1, y1) = atelier_core::raster::resolve_region(region, cw as u32, ch as u32)?;
        let (added, removed, recolored, bbox, ia, ib) =
            doc.frame_diff_region(frame_a, frame_b, layer, (x0, y0, x1, y1))?;
        let changed = added + removed + recolored;
        let mut res = json!({
            "changed": changed,
            "added": added,
            "removed": removed,
            "recolored": recolored,
            "change_bbox": bbox.map(|b| json!(b)).unwrap_or(Value::Null),
        });
        if grid {
            // Reuse doc_dump_region's 4096-px area cap so the grid stays readable.
            area_cap_check(
                "diff region",
                (x1 - x0 + 1) as u64,
                (y1 - y0 + 1) as u64,
                "pass a smaller `region` for grid=true",
            )?;
            let rows: Vec<String> = (y0..=y1)
                .map(|y| {
                    (x0..=x1)
                        .map(|x| {
                            let pa = ia.get_pixel(x as u32, y as u32).0;
                            let pb = ib.get_pixel(x as u32, y as u32).0;
                            if pa == pb {
                                '.'
                            } else {
                                match (pa[3] > 0, pb[3] > 0) {
                                    (false, true) => '+',
                                    (true, false) => '-',
                                    _ => '~',
                                }
                            }
                        })
                        .collect()
                })
                .collect();
            res["grid"] = json!(rows);
            res["origin"] = json!([x0, y0]);
        }
        let mut png = None;
        if render == "overlay" {
            let out = match out_path {
                Some(p) => PathBuf::from(p),
                None => dir.join(format!("diff_{}_{}.png", frame_a, frame_b)),
            };
            if let Some(p) = out.parent() {
                fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
            }
            // frame_b dimmed to 40%, then changed pixels in the region flagged.
            let mut img = RgbaImage::from_pixel(cw as u32, ch as u32, Rgba([0, 0, 0, 0]));
            for (x, y, p) in ib.enumerate_pixels() {
                if p.0[3] == 0 {
                    continue;
                }
                let a = (p.0[3] as u32 * 40 / 100) as u8;
                img.put_pixel(x, y, Rgba([p.0[0], p.0[1], p.0[2], a]));
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let pa = ia.get_pixel(x as u32, y as u32).0;
                    let pb = ib.get_pixel(x as u32, y as u32).0;
                    if pa == pb {
                        continue;
                    }
                    let flag = match (pa[3] > 0, pb[3] > 0) {
                        (false, true) => [0, 230, 0, 255], // added → green
                        (true, false) => [230, 0, 0, 255], // removed → red
                        _ => [230, 230, 0, 255],           // recoloured → yellow
                    };
                    img.put_pixel(x as u32, y as u32, Rgba(flag));
                }
            }
            let sc = scale.max(1);
            if sc > 1 {
                img = image::imageops::resize(
                    &img,
                    cw as u32 * sc,
                    ch as u32 * sc,
                    image::imageops::FilterType::Nearest,
                );
            }
            img.save(&out).map_err(|e| e.to_string())?;
            res["path"] = json!(out.to_string_lossy());
            png = Some(crate::encode_png(&img)?);
        } else if render != "none" {
            return Err(format!("unknown render '{}' — use none|overlay", render));
        }
        Ok((png, res))
    }

    /// Tiling seam report: wrap-test the far edge against the near edge for the
    /// requested `axis` ("horizontal" tests left↔right, "vertical" top↔bottom,
    /// "both" runs each). `threshold` is the max per-channel delta still counted
    /// a match. When any edge pixel mismatches (or `out_path` is given) an
    /// overlay PNG — frame dimmed, mismatched EDGE pixels painted red — is
    /// returned as bytes for the MCP layer to inline (and written to `out_path`
    /// when given). Per axis: `{mismatches, max_delta, worst:[[x,y,delta] ≤10]}`.
    pub fn doc_seam_report(
        &self,
        id: &str,
        layer: Option<usize>,
        frame: usize,
        axis: &str,
        threshold: i32,
        out_path: Option<&str>,
    ) -> Result<(Option<Vec<u8>>, Value), String> {
        use image::{Rgba, RgbaImage};
        let (_dir, doc) = self.open(id)?;
        let (want_h, want_v) = match axis {
            "both" => (true, true),
            "horizontal" => (true, false),
            "vertical" => (false, true),
            other => {
                return Err(format!(
                    "unknown axis '{}' — use both|horizontal|vertical",
                    other
                ))
            }
        };
        // One flatten serves both axes AND the overlay below.
        let img = doc.analysis_image(layer, frame)?;
        let report = |horizontal: bool| -> (Value, Vec<[i32; 3]>) {
            let (mismatches, max_delta, worst) =
                atelier_core::document::seam_axis_img(&img, horizontal, threshold);
            let worst_json: Vec<Value> = worst.iter().map(|w| json!(w)).collect();
            (
                json!({"mismatches": mismatches, "max_delta": max_delta, "worst": worst_json}),
                worst,
            )
        };
        let mut out = json!({});
        let mut flagged: Vec<[i32; 3]> = Vec::new();
        if want_h {
            let (j, w) = report(true);
            out["horizontal"] = j;
            flagged.extend(w);
        }
        if want_v {
            let (j, w) = report(false);
            out["vertical"] = j;
            flagged.extend(w);
        }
        let mut png = None;
        if out_path.is_some() || !flagged.is_empty() {
            // Dim the art to 40%, then paint the (capped) worst edge cells red.
            let mut canvas = RgbaImage::from_pixel(img.width(), img.height(), Rgba([0, 0, 0, 0]));
            for (x, y, px) in img.enumerate_pixels() {
                if px.0[3] == 0 {
                    continue;
                }
                let a = (px.0[3] as u32 * 40 / 100) as u8;
                canvas.put_pixel(x, y, Rgba([px.0[0], px.0[1], px.0[2], a]));
            }
            for w in &flagged {
                canvas.put_pixel(w[0] as u32, w[1] as u32, Rgba([255, 0, 0, 255]));
            }
            let sc = crate::preview_scale(canvas.width(), canvas.height());
            let scaled = if sc > 1 {
                image::imageops::resize(
                    &canvas,
                    canvas.width() * sc,
                    canvas.height() * sc,
                    image::imageops::FilterType::Nearest,
                )
            } else {
                canvas
            };
            if let Some(p) = out_path {
                let out_p = PathBuf::from(p);
                if let Some(parent) = out_p.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                }
                scaled.save(&out_p).map_err(|e| e.to_string())?;
                out["path"] = json!(out_p.to_string_lossy());
            }
            png = Some(crate::encode_png(&scaled)?);
        }
        Ok((png, out))
    }

    /// Audit an animation. mode="seam" diffs the wrap the loop actually plays
    /// (last→first for forward, first→last for reverse; pingpong has no seam →
    /// score 0 + note) and reports seam_score = changed/opaque. mode="spacing"
    /// tracks the silhouette bbox-centre per played frame and reports the
    /// per-frame offsets, total drift and evenness (stddev of step magnitude /
    /// mean — 0 = mechanically even). `tag` None audits the whole timeline.
    pub fn doc_anim_audit(
        &self,
        id: &str,
        tag: Option<&str>,
        layer: Option<usize>,
        mode: &str,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let seq = doc.play_sequence(tag)?;
        if seq.is_empty() {
            return Err("animation has no frames to audit".into());
        }
        match mode {
            "seam" => {
                // Pingpong reverses at the ends, so the loop never hard-cuts.
                let dir = match tag {
                    Some(name) => doc
                        .meta
                        .tags
                        .iter()
                        .find(|t| t.name == name)
                        .map(|t| t.direction.as_str())
                        .unwrap_or("forward"),
                    None => "forward",
                };
                if dir == "pingpong" {
                    return Ok(json!({
                        "seam_score": 0.0,
                        "note": "pingpong loops reverse at the ends — no last→first seam",
                    }));
                }
                let (last, first) = (*seq.last().unwrap(), seq[0]);
                let (cw, ch) = (doc.meta.w as i32, doc.meta.h as i32);
                let (added, removed, recolored, bbox, ia, _ib) =
                    doc.frame_diff_region(last, first, layer, (0, 0, cw - 1, ch - 1))?;
                let changed = added + removed + recolored;
                // Denominator: opaque pixels of the played last frame (motion
                // base), counted from the flatten the diff already produced.
                let opaque = ia.pixels().filter(|p| p.0[3] > 0).count().max(1) as u64;
                let seam_score = (changed as f64 / opaque as f64 * 1000.0).round() / 1000.0;
                Ok(json!({
                    "seam_score": seam_score,
                    "changed": changed,
                    "added": added,
                    "removed": removed,
                    // WHERE the loop pops — fix this area, not the whole frame.
                    "change_bbox": bbox.map(|b| json!(b)).unwrap_or(Value::Null),
                    "frames": [last, first],
                }))
            }
            "spacing" => {
                // Centre per played frame; offsets are step-to-step deltas.
                // A frame whose silhouette has no opaque pixel in the region
                // stays None — scoring it as motion to the canvas origin
                // poisoned drift/evenness whenever a part swung out of a
                // fixed region rect.
                let mut centers: Vec<Option<[f64; 2]>> = Vec::with_capacity(seq.len());
                for &f in &seq {
                    centers.push(doc.silhouette_center(layer, f, region)?);
                }
                let round1 = |v: f64| (v * 10.0).round() / 10.0;
                let per_frame_center: Vec<Value> = centers
                    .iter()
                    .map(|c| match c {
                        Some(c) => json!([round1(c[0]), round1(c[1])]),
                        None => Value::Null,
                    })
                    .collect();
                let empty_frames: Vec<usize> = centers
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.is_none())
                    .map(|(i, _)| seq[i])
                    .collect();
                // Offsets only between consecutive frames that BOTH have a
                // centre; the per-frame list keeps nulls in place.
                let mut offsets: Vec<[f64; 2]> = Vec::new();
                let per_frame_offset: Vec<Value> = centers
                    .windows(2)
                    .map(|w| match (w[0], w[1]) {
                        (Some(a), Some(b)) => {
                            let o = [b[0] - a[0], b[1] - a[1]];
                            offsets.push(o);
                            json!([round1(o[0]), round1(o[1])])
                        }
                        _ => Value::Null,
                    })
                    .collect();
                let known: Vec<[f64; 2]> = centers.iter().flatten().copied().collect();
                let total = match (known.first(), known.last()) {
                    (Some(a), Some(b)) => [round1(b[0] - a[0]), round1(b[1] - a[1])],
                    _ => [0.0, 0.0],
                };
                // Evenness: stddev of |offset| over its mean (0 = perfectly even).
                let mags: Vec<f64> = offsets
                    .iter()
                    .map(|o| (o[0] * o[0] + o[1] * o[1]).sqrt())
                    .collect();
                let evenness = if mags.is_empty() {
                    0.0
                } else {
                    let mean = mags.iter().sum::<f64>() / mags.len() as f64;
                    if mean < f64::EPSILON {
                        0.0
                    } else {
                        let var = mags.iter().map(|m| (m - mean).powi(2)).sum::<f64>()
                            / mags.len() as f64;
                        (var.sqrt() / mean * 1000.0).round() / 1000.0
                    }
                };
                let mut out = json!({
                    "per_frame_center": per_frame_center,
                    "per_frame_offset": per_frame_offset,
                    "total_drift": total,
                    "evenness": evenness,
                });
                if !empty_frames.is_empty() {
                    out["frames_without_center"] = json!(empty_frames);
                    out["note"] = json!(
                        "some frames have no opaque pixels in the region — skipped, not \
                         scored; widen the region if the part swings past it"
                    );
                }
                Ok(out)
            }
            "arc" => {
                // Trajectory shape: does the centre follow an arc (good for
                // jumps/swings) or a straight slide? Plus volume constancy —
                // squash/stretch should trade height for width, not vanish.
                // Frames with no centre in the region are skipped, matching
                // spacing mode — a [0,0] fallback faked a dive to the origin.
                let mut centers: Vec<[f64; 2]> = Vec::with_capacity(seq.len());
                let mut areas: Vec<f64> = Vec::with_capacity(seq.len());
                let mut skipped: Vec<usize> = Vec::new();
                for &f in &seq {
                    match doc.silhouette_stats(layer, f, region)? {
                        Some((c, opaque)) => {
                            centers.push(c);
                            areas.push(opaque as f64);
                        }
                        None => skipped.push(f),
                    }
                }
                if centers.len() < 2 {
                    return Err(
                        "fewer than two frames have opaque pixels in the region — widen it".into(),
                    );
                }
                // RMS perpendicular distance of the centres from the straight
                // line joining the first and last centre (0 = dead straight).
                let (a, b) = (centers[0], *centers.last().unwrap());
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let len = (dx * dx + dy * dy).sqrt();
                let arc_residual = if len < 1e-6 || centers.len() < 3 {
                    0.0
                } else {
                    let (nx, ny) = (-dy / len, dx / len); // unit normal
                    let ss: f64 = centers
                        .iter()
                        .map(|c| ((c[0] - a[0]) * nx + (c[1] - a[1]) * ny).powi(2))
                        .sum();
                    ((ss / centers.len() as f64).sqrt() * 100.0).round() / 100.0
                };
                let (vol_mean, vol_cv) = {
                    let m = areas.iter().sum::<f64>() / areas.len() as f64;
                    let cv = if m < f64::EPSILON {
                        0.0
                    } else {
                        let var =
                            areas.iter().map(|a| (a - m).powi(2)).sum::<f64>() / areas.len() as f64;
                        (var.sqrt() / m * 1000.0).round() / 1000.0
                    };
                    (m.round(), cv)
                };
                let mut out = json!({
                    "trajectory": centers.iter().map(|c| json!([(c[0]*10.0).round()/10.0, (c[1]*10.0).round()/10.0])).collect::<Vec<_>>(),
                    "arc_residual": arc_residual,
                    "shape": if arc_residual < 1.0 { "straight" } else { "arced" },
                    "volume_mean": vol_mean,
                    "volume_cv": vol_cv,
                    "note": "arc_residual ~0 = straight slide (often too mechanical for jumps/swings); volume_cv near 0 = constant mass (good unless deliberate squash)",
                });
                if !skipped.is_empty() {
                    out["frames_without_center"] = json!(skipped);
                }
                Ok(out)
            }
            "timing" => {
                let durs: Vec<u32> = seq
                    .iter()
                    .map(|&f| doc.meta.frames[f].duration_ms)
                    .collect();
                let total: u32 = durs.iter().sum();
                let uniform = durs.windows(2).all(|w| w[0] == w[1]);
                let mut out = json!({
                    "per_frame_ms": durs,
                    "total_ms": total,
                    "uniform": uniform,
                });
                if uniform && seq.len() > 2 {
                    out["note"] = json!(
                        "uniform timing reads mechanical — hold contact/key poses ~1.5x longer \
                         with doc_set_frame_duration"
                    );
                }
                Ok(out)
            }
            other => Err(format!(
                "unknown mode '{}' — use seam|spacing|arc|timing",
                other
            )),
        }
    }

    /// Translucency report — makes glass / glow / soft FX measurable instead of
    /// eyeballed. Over the flattened frame (or one `layer`, `region`-clipped):
    /// counts fully-opaque / partial (0<a<255) / transparent pixels, the mean
    /// alpha of non-transparent pixels, a coarse alpha-band histogram, and the
    /// bbox of the partially-transparent pixels.
    pub fn doc_translucency_report(
        &self,
        id: &str,
        frame: usize,
        layer: Option<usize>,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let img = doc.analysis_image(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let (ax, ay, bx, by) = atelier_core::raster::resolve_region(region, w as u32, h as u32)?;
        let (mut opaque, mut partial, mut transparent) = (0u64, 0u64, 0u64);
        let (mut sum_a, mut nz) = (0u64, 0u64);
        let mut bands = [0u64; 4]; // 1-64, 65-128, 129-192, 193-254
        let mut bbox: Option<[i32; 4]> = None;
        for y in ay..=by {
            for x in ax..=bx {
                let a = img.get_pixel(x as u32, y as u32).0[3];
                match a {
                    0 => transparent += 1,
                    255 => {
                        opaque += 1;
                        sum_a += 255;
                        nz += 1;
                    }
                    _ => {
                        partial += 1;
                        sum_a += a as u64;
                        nz += 1;
                        bands[(a as usize - 1) / 64] += 1;
                        bbox = Some(match bbox {
                            None => [x, y, x, y],
                            Some([a0, b0, c0, d0]) => [a0.min(x), b0.min(y), c0.max(x), d0.max(y)],
                        });
                    }
                }
            }
        }
        Ok(json!({
            "doc_id": id, "frame": frame,
            "opaque": opaque, "partial": partial, "transparent": transparent,
            "mean_alpha_nonzero": if nz > 0 { (sum_a as f64 / nz as f64).round() as u32 } else { 0 },
            "partial_alpha_bands": {"1-64": bands[0], "65-128": bands[1], "129-192": bands[2], "193-254": bands[3]},
            "partial_bbox": bbox.map(|b| json!({"x": b[0], "y": b[1], "w": b[2]-b[0]+1, "h": b[3]-b[1]+1})),
            "note": "partial pixels are soft/AA/glow edges; many over a glass shape = readable translucency, scattered specks = stray AA",
        }))
    }

    /// Colour-vision-deficiency audit: simulate protanopia / deuteranopia /
    /// tritanopia over the flattened frame (Machado 2009 severity-1.0 matrices
    /// in linear RGB) and report which of the art's distinct colour pairs —
    /// distinguishable to typical vision — COLLAPSE under each simulation
    /// (OKLab ΔE drops below the readable floor). Returns a side-by-side strip
    /// (normal · protan · deutan · tritan) plus the collapsing pairs, so
    /// "is my health bar readable to 8% of players?" becomes a tool call.
    pub fn doc_colorblind_check(
        &self,
        id: &str,
        frame: usize,
        scale: u32,
    ) -> Result<(Vec<u8>, Value), String> {
        const KINDS: [(&str, [[f32; 3]; 3]); 3] = [
            (
                "protanopia",
                [
                    [0.152286, 1.052583, -0.204868],
                    [0.114503, 0.786281, 0.099216],
                    [-0.003882, -0.048116, 1.051998],
                ],
            ),
            (
                "deuteranopia",
                [
                    [0.367322, 0.860646, -0.227968],
                    [0.280085, 0.672501, 0.047413],
                    [-0.011820, 0.042940, 0.968881],
                ],
            ),
            (
                "tritanopia",
                [
                    [1.255528, -0.076749, -0.178779],
                    [-0.078411, 0.930809, 0.147602],
                    [0.004733, 0.691367, 0.303900],
                ],
            ),
        ];
        let lin = |c: u8| {
            let v = c as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let unlin = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            let s = if v <= 0.0031308 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (s * 255.0).round().clamp(0.0, 255.0) as u8
        };
        let simulate = |p: [u8; 4], m: &[[f32; 3]; 3]| -> [u8; 4] {
            let (r, g, b) = (lin(p[0]), lin(p[1]), lin(p[2]));
            [
                unlin(m[0][0] * r + m[0][1] * g + m[0][2] * b),
                unlin(m[1][0] * r + m[1][1] * g + m[1][2] * b),
                unlin(m[2][0] * r + m[2][1] * g + m[2][2] * b),
                p[3],
            ]
        };
        let (_dir, doc) = self.open(id)?;
        let img = doc.analysis_image(None, frame)?;
        let (w, h) = (img.width(), img.height());
        // Distinct colours by frequency (capped: pair analysis is quadratic).
        let mut freq: std::collections::HashMap<[u8; 4], u64> = std::collections::HashMap::new();
        for p in img.pixels() {
            if p.0[3] > 0 {
                *freq.entry(p.0).or_insert(0) += 1;
            }
        }
        let mut colors: Vec<[u8; 4]> = freq.keys().copied().collect();
        colors.sort_by_key(|c| std::cmp::Reverse(freq[c]));
        colors.truncate(24);
        let de = |a: [u8; 4], b: [u8; 4]| {
            let (l1, a1, b1) = atelier_core::raster::srgb_to_oklab(a);
            let (l2, a2, b2) = atelier_core::raster::srgb_to_oklab(b);
            ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
        };
        let hex = |c: [u8; 4]| crate::hex_rgb(&c);
        let mut report: Vec<Value> = Vec::new();
        let mut total_collapsed = 0usize;
        // The strip: normal + the three simulations, side by side, 1px gutter.
        let gut = 1u32;
        let mut strip = image::RgbaImage::from_pixel(w * 4 + gut * 3, h, image::Rgba([0, 0, 0, 0]));
        image::imageops::replace(&mut strip, &img, 0, 0);
        for (k, (name, m)) in KINDS.iter().enumerate() {
            let mut sim = image::RgbaImage::new(w, h);
            for (x, y, p) in img.enumerate_pixels() {
                sim.put_pixel(x, y, image::Rgba(simulate(p.0, m)));
            }
            image::imageops::replace(&mut strip, &sim, ((k as u32 + 1) * (w + gut)) as i64, 0);
            let mut collapsed: Vec<Value> = Vec::new();
            for i in 0..colors.len() {
                for j in (i + 1)..colors.len() {
                    let (a, b) = (colors[i], colors[j]);
                    // Readable apart normally, unreadable under the simulation.
                    if de(a, b) >= 0.08 && de(simulate(a, m), simulate(b, m)) < 0.05 {
                        collapsed.push(json!({"a": hex(a), "b": hex(b)}));
                    }
                }
            }
            total_collapsed += collapsed.len();
            report.push(json!({"kind": name, "collapsing_pairs": collapsed}));
        }
        let sc = scale.clamp(1, 8);
        let out = if sc > 1 {
            image::imageops::resize(
                &strip,
                strip.width() * sc,
                strip.height() * sc,
                image::imageops::FilterType::Nearest,
            )
        } else {
            strip
        };
        let png = super::encode_png(&out)?;
        let value = json!({
            "doc_id": id,
            "frame": frame,
            "colors_checked": colors.len(),
            "simulations": report,
            "strip": "normal · protanopia · deuteranopia · tritanopia",
            "verdict": if total_collapsed == 0 { "ok" } else { "warn" },
        });
        Ok((png, value))
    }
}

/// Mean angle and max angular spread (degrees) of a set of directions, handling
/// the 0/360 wrap via unit-vector summation. Empty → `(None, None)`.
fn circular_summary(deg: &[f64]) -> (Option<f64>, Option<f64>) {
    if deg.is_empty() {
        return (None, None);
    }
    let (mut sx, mut sy) = (0.0, 0.0);
    for &d in deg {
        let r = d.to_radians();
        sx += r.cos();
        sy += r.sin();
    }
    let mean = sy.atan2(sx).to_degrees();
    let spread = deg
        .iter()
        .map(|&d| {
            let mut diff = (d - mean).rem_euclid(360.0);
            if diff > 180.0 {
                diff -= 360.0;
            }
            diff.abs()
        })
        .fold(0.0f64, f64::max);
    (Some(mean), Some(spread))
}

/// The guts of `doc_form_audit`, factored out so it can be unit-tested without a
/// Studio/disk. For each connected opaque component it fits a lightness plane
/// (the inferred light direction), correlates lightness with interior distance
/// (the pillow-shading tell), and reports whether the forms share one light.
pub(super) fn form_audit_image(img: &image::RgbaImage, min_area: u32) -> Value {
    use atelier_core::raster;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let fg: Vec<bool> = img.pixels().map(|p| p.0[3] > 0).collect();
    let idist = raster::interior_distance(&fg, w, h);
    let idx = |x: usize, y: usize| y * w + x;
    let neigh: [(i32, i32); 8] = [
        (-1, 0),
        (1, 0),
        (0, -1),
        (0, 1),
        (-1, -1),
        (1, -1),
        (-1, 1),
        (1, 1),
    ];
    let mut seen = vec![false; w * h];
    let mut forms: Vec<Value> = Vec::new();
    let mut azimuths: Vec<f64> = Vec::new();
    let mut pillow_count = 0u32;
    for sy in 0..h {
        for sx in 0..w {
            let si = idx(sx, sy);
            if seen[si] || !fg[si] {
                continue;
            }
            // DFS the component (Vec-as-stack), gathering (x, y, lightness, interior-distance).
            let mut stack = vec![(sx, sy)];
            seen[si] = true;
            let mut px: Vec<(f64, f64, f64, f64)> = Vec::new();
            let mut bbox = [sx as i32, sy as i32, sx as i32, sy as i32];
            while let Some((x, y)) = stack.pop() {
                let l = raster::srgb_to_oklab(img.get_pixel(x as u32, y as u32).0).0 as f64;
                px.push((x as f64, y as f64, l, idist[idx(x, y)] as f64));
                bbox[0] = bbox[0].min(x as i32);
                bbox[1] = bbox[1].min(y as i32);
                bbox[2] = bbox[2].max(x as i32);
                bbox[3] = bbox[3].max(y as i32);
                for (ox, oy) in neigh {
                    let (nx, ny) = (x as i32 + ox, y as i32 + oy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let ni = idx(nx as usize, ny as usize);
                    if !seen[ni] && fg[ni] {
                        seen[ni] = true;
                        stack.push((nx as usize, ny as usize));
                    }
                }
            }
            let area = px.len() as u32;
            if area < min_area.max(4) {
                continue;
            }
            let n = px.len() as f64;
            let (mx, my, ml) = (
                px.iter().map(|p| p.0).sum::<f64>() / n,
                px.iter().map(|p| p.1).sum::<f64>() / n,
                px.iter().map(|p| p.2).sum::<f64>() / n,
            );
            // Least-squares plane L ≈ a·x + b·y + c on centred coords: (a, b) is
            // the lightness gradient, pointing toward the light.
            let (mut sxx, mut syy, mut sxy, mut sxl, mut syl) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for &(x, y, l, _) in &px {
                let (dx, dy, dl) = (x - mx, y - my, l - ml);
                sxx += dx * dx;
                syy += dy * dy;
                sxy += dx * dy;
                sxl += dx * dl;
                syl += dy * dl;
            }
            let det = sxx * syy - sxy * sxy;
            let (a, b) = if det.abs() > 1e-6 {
                ((sxl * syy - syl * sxy) / det, (syl * sxx - sxl * sxy) / det)
            } else {
                (0.0, 0.0)
            };
            let sll: f64 = px.iter().map(|&(_, _, l, _)| (l - ml).powi(2)).sum();
            let ss_res: f64 = px
                .iter()
                .map(|&(x, y, l, _)| (l - (a * (x - mx) + b * (y - my) + ml)).powi(2))
                .sum();
            let plane_r2 = if sll > 1e-9 {
                (1.0 - ss_res / sll).max(0.0)
            } else {
                0.0
            };
            // Pillow tell: lightness correlated with distance-to-edge (bright
            // centre, dark all round) rather than a direction.
            let md = px.iter().map(|p| p.3).sum::<f64>() / n;
            let (mut cov, mut vl, mut vd) = (0.0, 0.0, 0.0);
            for &(_, _, l, d) in &px {
                let (dl, dd) = (l - ml, d - md);
                cov += dl * dd;
                vl += dl * dl;
                vd += dd * dd;
            }
            let pillow_corr = if vl > 1e-9 && vd > 1e-9 {
                cov / (vl.sqrt() * vd.sqrt())
            } else {
                0.0
            };
            let mag = (a * a + b * b).sqrt();
            // Image y is down; negate it so azimuth reads maths-standard
            // (0° = right, 90° = up).
            let azimuth = if mag > 1e-6 {
                (-b).atan2(a).to_degrees()
            } else {
                f64::NAN
            };
            let directional = plane_r2 >= 0.4 && mag > 1e-4;
            let is_pillow = pillow_corr > 0.5 && plane_r2 < 0.35;
            if is_pillow {
                pillow_count += 1;
            }
            if directional && !azimuth.is_nan() {
                azimuths.push(azimuth);
            }
            let verdict = if is_pillow {
                "pillow"
            } else if directional {
                "directional"
            } else {
                "flat"
            };
            forms.push(json!({
                "bbox": bbox,
                "area": area,
                "light_azimuth_deg": if azimuth.is_nan() { Value::Null } else { json!(azimuth.round()) },
                "plane_fit_r2": (plane_r2 * 100.0).round() / 100.0,
                "pillow_corr": (pillow_corr * 100.0).round() / 100.0,
                "verdict": verdict,
            }));
        }
    }
    let (dominant, spread) = circular_summary(&azimuths);
    let inconsistent = azimuths.len() >= 2 && spread.map(|s| s > 45.0).unwrap_or(false);
    let summary_verdict = if pillow_count > 0 {
        "pillow-shading detected"
    } else if inconsistent {
        "inconsistent light direction"
    } else if forms.is_empty() {
        "no forms"
    } else {
        "ok"
    };
    json!({
        "forms": forms,
        "pillow_forms": pillow_count,
        "dominant_light_azimuth_deg": dominant.map(|d| json!(d.round())).unwrap_or(Value::Null),
        "light_spread_deg": spread.map(|s| json!((s * 10.0).round() / 10.0)).unwrap_or(Value::Null),
        "verdict": summary_verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::Studio;
    use super::{circular_summary, form_audit_image};
    use serde_json::{json, Value};

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-test-{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn dump_region_symbol_and_hex() {
        let s = studio("dump");
        s.doc_create("d", 4, 4).unwrap();
        // two distinct opaque pixels, rest transparent
        s.doc_pencil("d", 0, 0, vec![(0, 0)], [10, 20, 30, 255], 1)
            .unwrap();
        s.doc_pencil("d", 0, 0, vec![(1, 0)], [40, 50, 60, 255], 1)
            .unwrap();
        let sym = s
            .doc_dump_region("d", 0, None, Some((0, 0, 1, 0)), "symbol")
            .unwrap();
        assert_eq!(sym["rows"][0], "AB"); // first-seen order
        assert_eq!(sym["legend"]["A"], "#0a141eff");
        let hx = s
            .doc_dump_region("d", 0, None, Some((0, 0, 2, 0)), "hex")
            .unwrap();
        assert_eq!(hx["rows"][0], "#0a141e #28323c ."); // opaque, opaque, transparent
                                                        // area cap rejects oversized regions
        s.doc_create("big", 128, 128).unwrap();
        assert!(s.doc_dump_region("big", 0, None, None, "symbol").is_err());
    }

    #[test]
    fn silhouette_reports_bbox_and_fill() {
        let s = studio("silo");
        s.doc_create("d", 4, 4).unwrap();
        s.doc_rect("d", 0, 0, 1, 1, 2, 2, [9, 9, 9, 255], true, 1)
            .unwrap();
        let r = s.doc_silhouette("d", 0, None, 1).unwrap();
        assert_eq!(r["bbox"], json!([1, 1, 2, 2])); // 2x2 block
        assert_eq!(r["fill_ratio"], json!(0.25)); // 4 of 16 opaque
        assert_eq!(r["grid"][0], "...."); // empty top row
        assert_eq!(r["grid"][1], ".##."); // the block's first row
    }

    #[test]
    fn colorblind_check_flags_red_green_collapse() {
        let s = studio("cvd");
        s.doc_create("ui", 8, 4).unwrap();
        // Same-lightness red and green blocks: readable normally, classic
        // protan/deutan collapse.
        s.doc_rect("ui", 0, 0, 0, 0, 3, 3, [200, 60, 60, 255], true, 1)
            .unwrap();
        s.doc_rect("ui", 0, 0, 4, 0, 7, 3, [80, 150, 60, 255], true, 1)
            .unwrap();
        let (png, r) = s.doc_colorblind_check("ui", 0, 1).unwrap();
        assert!(!png.is_empty());
        assert_eq!(r["verdict"], "warn");
        let sims = r["simulations"].as_array().unwrap();
        let collapsed: usize = sims
            .iter()
            .filter(|k| {
                (k["kind"] == "protanopia" || k["kind"] == "deuteranopia")
                    && !k["collapsing_pairs"].as_array().unwrap().is_empty()
            })
            .count();
        assert!(
            collapsed >= 1,
            "red/green should collapse under protan or deutan"
        );
    }

    #[test]
    fn form_audit_flags_directional_light() {
        // A padded 12x12 block whose lightness ramps along +x reads as a clean
        // directional fit, not pillow.
        let mut img = image::RgbaImage::new(16, 16);
        for y in 2..14u32 {
            for x in 2..14u32 {
                let v = (40 + (x - 2) * 16).min(255) as u8;
                img.put_pixel(x, y, image::Rgba([v, v, v, 255]));
            }
        }
        let r = form_audit_image(&img, 12);
        assert_eq!(r["forms"][0]["verdict"], "directional");
        assert_eq!(r["pillow_forms"], 0);
        assert_eq!(r["verdict"], "ok");
    }

    #[test]
    fn form_audit_flags_pillow_shading() {
        // A padded 12x12 block lit concentrically (bright centre, dark all
        // edges) — the classic pillow-shading tell.
        let mut img = image::RgbaImage::new(16, 16);
        for y in 2..14i32 {
            for x in 2..14i32 {
                let (lx, ly) = (x - 2, y - 2);
                let edge = lx.min(ly).min(11 - lx).min(11 - ly); // dist to block edge
                let v = (40 + edge * 26).min(255) as u8;
                img.put_pixel(x as u32, y as u32, image::Rgba([v, v, v, 255]));
            }
        }
        let r = form_audit_image(&img, 12);
        assert_eq!(r["forms"][0]["verdict"], "pillow");
        assert_eq!(r["pillow_forms"], 1);
        assert_eq!(r["verdict"], "pillow-shading detected");
    }

    #[test]
    fn circular_summary_wraps_across_zero() {
        let (mean, spread) = circular_summary(&[350.0, 10.0]);
        let m = mean.unwrap().rem_euclid(360.0);
        assert!(
            !(1.0..=359.0).contains(&m),
            "mean {m} should sit near 0/360"
        );
        assert!((spread.unwrap() - 10.0).abs() < 0.5);
    }

    #[test]
    fn components_counts_blobs_and_specks() {
        let s = studio("comp");
        s.doc_create("d", 8, 8).unwrap();
        // a 3x3 blob and a single stray speck, well separated
        s.doc_rect("d", 0, 0, 0, 0, 2, 2, [255, 0, 0, 255], true, 1)
            .unwrap();
        s.doc_pencil("d", 0, 0, vec![(7, 7)], [0, 255, 0, 255], 1)
            .unwrap();
        let r = s.doc_components("d", 0, None, 8, None, 1).unwrap();
        assert_eq!(r["count"], 2);
        assert_eq!(r["components"][0]["area"], 9); // biggest first
        assert_eq!(r["components"][0]["dominant"], "#ff0000");
        assert_eq!(r["specks"].as_array().unwrap().len(), 1); // the 1px dot
                                                              // colour filter isolates one blob
        let red = s
            .doc_components("d", 0, None, 8, Some([255, 0, 0, 255]), 1)
            .unwrap();
        assert_eq!(red["count"], 1);
    }

    #[test]
    fn coverage_map_grid_and_offset() {
        let s = studio("cov");
        s.doc_create("d", 8, 8).unwrap();
        // fill the top-left quadrant solid white
        s.doc_rect("d", 0, 0, 0, 0, 3, 3, [255, 255, 255, 255], true, 1)
            .unwrap();
        let r = s.doc_coverage_map("d", 0, 2, 2).unwrap();
        assert_eq!(r["grid"][0][0]["fill"], json!(1.0)); // full cell
        assert_eq!(r["grid"][0][0]["value"], json!(255)); // white luma
        assert_eq!(r["grid"][1][1]["fill"], json!(0.0)); // empty cell
        assert_eq!(r["grid"][1][1]["value"], Value::Null);
        assert_eq!(r["content_bbox"], json!([0, 0, 3, 3]));
        // content centres up-left of canvas centre → negative offsets
        assert_eq!(r["center_offset"], json!([-2.0, -2.0]));
    }

    #[test]
    fn look_grayscale_writes_file_and_reports() {
        let s = studio("renderval");
        s.doc_create("d", 4, 4).unwrap();
        // one black-ish and one white pixel; rest transparent
        s.doc_pencil("d", 0, 0, vec![(0, 0)], [0, 0, 0, 255], 1)
            .unwrap();
        s.doc_pencil("d", 0, 0, vec![(1, 0)], [255, 255, 255, 255], 1)
            .unwrap();
        let out = s.docs_dir.join("val.png");
        let (png, r) = s
            .look(
                "d",
                0,
                Some(1),
                None,
                "grayscale",
                4,
                false,
                false,
                false,
                None,
                None,
                out.to_str(),
            )
            .unwrap();
        assert!(out.exists()); // out_path written
        assert!(!png.is_empty()); // inline preview bytes
        assert_eq!(r["native_size"], json!([4, 4])); // scale 1 keeps native size
        let v = &r["stats"]["value"];
        assert_eq!(v["min"], json!(0)); // black luma
        assert_eq!(v["max"], json!(255)); // white luma
        assert_eq!(v["mean"], json!(128)); // (0+255)/2 rounded
        assert_eq!(v["contrast"], json!(1.0)); // full value range
                                               // unknown mode is an actionable error
        assert!(s
            .look(
                "d",
                0,
                Some(1),
                None,
                "bogus",
                4,
                false,
                false,
                false,
                None,
                None,
                None,
            )
            .is_err());
    }

    #[test]
    fn contrast_check_modes() {
        let s = studio("contrast");
        s.doc_create("d", 8, 8).unwrap();
        // white inner block on a black surround → very high contrast
        s.doc_fill_cel("d", 0, 0, [0, 0, 0, 255]).unwrap();
        s.doc_rect("d", 0, 0, 3, 3, 4, 4, [255, 255, 255, 255], true, 1)
            .unwrap();
        let region = s
            .doc_contrast_check("d", 0, "region", Some((3, 3, 4, 4)), 1.5, 128, None)
            .unwrap()
            .1;
        assert_eq!(region["pass"], json!(true));
        assert!(region["ratio"].as_f64().unwrap() > 10.0); // black/white ≈ 21
                                                           // region mode without a region errors
        assert!(s
            .doc_contrast_check("d", 0, "region", None, 1.5, 128, None)
            .is_err());
        // palette mode: two distinct colours, one pair
        let pal = s
            .doc_contrast_check("d", 0, "palette", None, 1.5, 128, None)
            .unwrap()
            .1;
        assert_eq!(pal["colors"], json!(2));
        assert_eq!(pal["pairs"].as_array().unwrap().len(), 1);
        // one-bit renders a B/W png and splits coverage
        let out = s.docs_dir.join("onebit.png");
        let ob = s
            .doc_contrast_check("d", 0, "one-bit", None, 1.5, 128, out.to_str())
            .unwrap()
            .1;
        assert!(out.exists());
        assert!(ob["white_pct"].as_f64().unwrap() > 0.0);
        assert!(ob["black_pct"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn palette_report_counts_and_in_palette() {
        let s = studio("palrep");
        s.doc_create("d", 4, 4).unwrap();
        s.doc_set_palette("d", vec![[255, 0, 0, 255]]).unwrap();
        // 3 red (in palette) + 1 near-red green-stray (off palette)
        s.doc_rect("d", 0, 0, 0, 0, 2, 0, [255, 0, 0, 255], true, 1)
            .unwrap();
        s.doc_pencil("d", 0, 0, vec![(0, 1)], [250, 4, 0, 255], 1)
            .unwrap();
        let r = s.doc_palette_report("d", Some(0), None, None, 8).unwrap();
        assert_eq!(r["count"], 2);
        assert_eq!(r["colors"][0]["hex"], "#ff0000ff"); // most-used first
        assert_eq!(r["colors"][0]["in_palette"], json!(true));
        assert_eq!(r["off_palette_count"], json!(1)); // the near-red stray
                                                      // the two reds are within dist 8 → flagged as near-dupes
        assert_eq!(r["near_dupes"].as_array().unwrap().len(), 1);
        // no-palette doc → in_palette null
        s.doc_create("e", 2, 2).unwrap();
        s.doc_pencil("e", 0, 0, vec![(0, 0)], [1, 2, 3, 255], 1)
            .unwrap();
        let r2 = s.doc_palette_report("e", Some(0), None, None, 8).unwrap();
        assert_eq!(r2["colors"][0]["in_palette"], Value::Null);
        assert_eq!(r2["off_palette_count"], Value::Null);
    }

    #[test]
    fn ramp_validate_flags_reversal_and_direction() {
        let s = studio("ramp");
        // a clean dark→light ramp: monotonic, even-ish, warming hue
        let good = s
            .doc_ramp_validate(
                Some(vec![
                    [30, 20, 50, 255],
                    [120, 110, 90, 255],
                    [210, 200, 150, 255],
                ]),
                None,
                None,
            )
            .unwrap();
        assert_eq!(good["monotonic_value"], json!(true));
        assert_eq!(good["value_deltas"].as_array().unwrap().len(), 2);
        assert_eq!(good["even_spacing"], json!(true)); // ~87,88 luma steps
        assert!(good["warnings"].as_array().unwrap().is_empty());
        // a ramp whose value dips in the middle → reversal warning + non-monotonic
        let bad = s
            .doc_ramp_validate(
                Some(vec![
                    [10, 10, 10, 255],
                    [200, 200, 200, 255],
                    [80, 80, 80, 255],
                ]),
                None,
                None,
            )
            .unwrap();
        assert_eq!(bad["monotonic_value"], json!(false));
        assert!(bad["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("reverses")));
        // too few colours / no input → errors
        assert!(s
            .doc_ramp_validate(Some(vec![[1, 2, 3, 255]]), None, None)
            .is_err());
        assert!(s.doc_ramp_validate(None, None, None).is_err());
        // validate a doc's locked palette
        s.doc_create("p", 4, 4).unwrap();
        s.doc_set_palette("p", vec![[10, 10, 10, 255], [200, 200, 200, 255]])
            .unwrap();
        let dp = s.doc_ramp_validate(None, Some("p"), None).unwrap();
        assert_eq!(dp["count"], 2);
        assert_eq!(dp["monotonic_value"], json!(true));
    }

    #[test]
    fn frame_diff_classifies_changes_and_grids() {
        let s = studio("framediff");
        s.doc_create("d", 4, 4).unwrap();
        s.doc_add_frame("d", 100, Some(0)).unwrap(); // frame 1 copies frame 0
                                                     // frame 0: a red pixel at (0,0); frame 1: move it and recolour (1,1).
        s.doc_pencil("d", 0, 0, vec![(0, 0)], [255, 0, 0, 255], 1)
            .unwrap();
        s.doc_pencil("d", 0, 1, vec![(1, 1)], [0, 255, 0, 255], 1)
            .unwrap();
        let (png, r) = s
            .doc_frame_diff("d", 0, 1, None, None, true, "none", None, 1)
            .unwrap();
        assert!(png.is_none()); // render="none" → no inline overlay
                                // (0,0) opaque→transparent = removed; (1,1) transparent→opaque = added.
        assert_eq!(r["added"], json!(1));
        assert_eq!(r["removed"], json!(1));
        assert_eq!(r["recolored"], json!(0));
        assert_eq!(r["changed"], json!(2));
        assert_eq!(r["change_bbox"], json!([0, 0, 1, 1]));
        assert_eq!(r["grid"][0], "-..."); // (0,0) removed
        assert_eq!(r["grid"][1], ".+.."); // (1,1) added
                                          // overlay render writes a PNG and returns the bytes for inlining
        let out = s.docs_dir.join("diff.png");
        let (ov_png, ov) = s
            .doc_frame_diff("d", 0, 1, None, None, false, "overlay", out.to_str(), 1)
            .unwrap();
        assert!(out.exists());
        assert!(ov_png.is_some());
        assert_eq!(ov["path"], json!(out.to_string_lossy()));
        // unknown render mode is an actionable error
        assert!(s
            .doc_frame_diff("d", 0, 1, None, None, false, "bogus", None, 1)
            .is_err());
    }

    #[test]
    fn seam_report_finds_edge_mismatches() {
        let s = studio("seam");
        s.doc_create("d", 4, 4).unwrap();
        // A left column that does not match the right column → horizontal seam.
        s.doc_rect("d", 0, 0, 0, 0, 0, 3, [255, 0, 0, 255], true, 1)
            .unwrap();
        let (png, r) = s.doc_seam_report("d", None, 0, "both", 0, None).unwrap();
        assert!(png.is_some()); // mismatches → inline overlay
        assert!(r["horizontal"]["mismatches"].as_u64().unwrap() > 0);
        assert_eq!(r["vertical"]["mismatches"], json!(0)); // rows tile fine (all blank top/bottom)
        assert_eq!(r["horizontal"]["max_delta"], json!(255)); // opaque red vs transparent
                                                              // worst list reports far-edge cells (x = w-1 = 3)
        let worst = r["horizontal"]["worst"].as_array().unwrap();
        assert!(!worst.is_empty());
        assert_eq!(worst[0][0], json!(3));
        // a seamless cel (uniform fill) has zero mismatches on both axes
        s.doc_create("e", 4, 4).unwrap();
        s.doc_fill_cel("e", 0, 0, [10, 20, 30, 255]).unwrap();
        let (clean_png, clean) = s.doc_seam_report("e", None, 0, "both", 0, None).unwrap();
        assert!(clean_png.is_none()); // no mismatches → no overlay
        assert_eq!(clean["horizontal"]["mismatches"], json!(0));
        assert_eq!(clean["vertical"]["mismatches"], json!(0));
        // bad axis errors
        assert!(s
            .doc_seam_report("e", None, 0, "diagonal", 0, None)
            .is_err());
    }

    #[test]
    fn anim_audit_seam_and_spacing() {
        let s = studio("animaudit");
        s.doc_create("d", 8, 8).unwrap();
        // 3 frames: a 2x2 block stepping right by 2 each frame (even spacing).
        s.doc_rect("d", 0, 0, 0, 0, 1, 1, [9, 9, 9, 255], true, 1)
            .unwrap();
        s.doc_add_frame("d", 100, None).unwrap();
        s.doc_rect("d", 0, 1, 2, 0, 3, 1, [9, 9, 9, 255], true, 1)
            .unwrap();
        s.doc_add_frame("d", 100, None).unwrap();
        s.doc_rect("d", 0, 2, 4, 0, 5, 1, [9, 9, 9, 255], true, 1)
            .unwrap();
        // spacing: even rightward drift → low evenness, positive total drift.
        let sp = s.doc_anim_audit("d", None, None, "spacing", None).unwrap();
        assert_eq!(sp["per_frame_center"].as_array().unwrap().len(), 3);
        assert_eq!(sp["per_frame_offset"].as_array().unwrap().len(), 2);
        assert!(sp["total_drift"][0].as_f64().unwrap() > 0.0); // moved right
        assert_eq!(sp["evenness"], json!(0.0)); // two equal 2px steps
                                                // seam: last frame vs first differ → non-zero score
        let seam = s.doc_anim_audit("d", None, None, "seam", None).unwrap();
        assert!(seam["seam_score"].as_f64().unwrap() > 0.0);
        assert_eq!(seam["frames"], json!([2, 0]));
        // pingpong tag → no seam (score 0 + note)
        s.doc_add_tag("d", "pp", 0, 2, "pingpong").unwrap();
        let pp = s
            .doc_anim_audit("d", Some("pp"), None, "seam", None)
            .unwrap();
        assert_eq!(pp["seam_score"], json!(0.0));
        assert!(pp["note"].is_string());
        // bad mode errors
        assert!(s.doc_anim_audit("d", None, None, "bogus", None).is_err());
    }

    #[test]
    fn keyframe_move_eases_across_frames() {
        let s = studio("keyframe");
        s.doc_create("d", 16, 16).unwrap();
        // a 2x2 block at (1,1) on frame 0; two empty frames to animate into.
        s.doc_rect("d", 0, 0, 1, 1, 2, 2, [200, 50, 50, 255], true, 1)
            .unwrap();
        s.doc_add_frame("d", 100, None).unwrap();
        s.doc_add_frame("d", 100, None).unwrap();
        let r = s
            .doc_keyframe_move("d", 0, (1, 1, 2, 2), 0, 2, 8, 0, "linear", true)
            .unwrap();
        assert_eq!(r["frames_touched"], json!(2));
        assert_eq!(r["offsets"], json!([[4, 0], [8, 0]])); // linear: half then full
                                                           // frame 0 (source) is untouched
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 1, 1).unwrap()["rgba"],
            json!([200, 50, 50, 255])
        );
        // frame 2 has the block at (1+8, 1) = (9,1); the source rect is cleared
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 2, 9, 1).unwrap()["rgba"],
            json!([200, 50, 50, 255])
        );
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 2, 1, 1).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
        // to_frame must exist and be > from_frame
        assert!(s
            .doc_keyframe_move("d", 0, (1, 1, 2, 2), 0, 9, 8, 0, "linear", true)
            .is_err());
        assert!(s
            .doc_keyframe_move("d", 0, (1, 1, 2, 2), 2, 0, 8, 0, "linear", true)
            .is_err());
    }
}
