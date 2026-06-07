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
pub(super) fn area_cap_check(label: &str, w: u64, h: u64, advice: &str) -> Result<(), String> {
    let area = w * h;
    if area > 4096 {
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
        let (x0, y0, x1, y1) = match region {
            Some((a, b, c, d)) => (
                a.min(c).max(0),
                b.min(d).max(0),
                a.max(c).min(cw - 1),
                b.max(d).min(ch - 1),
            ),
            None => (0, 0, cw - 1, ch - 1),
        };
        if x0 > x1 || y0 > y1 {
            return Err("region is empty after clamping to the canvas".into());
        }
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
                                format!("#{:02x}{:02x}{:02x}", p[0], p[1], p[2])
                            } else {
                                format!("#{:02x}{:02x}{:02x}{:02x}", p[0], p[1], p[2], p[3])
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
            .map(|(i, c)| {
                (
                    (GLYPHS[i] as char).to_string(),
                    json!(format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])),
                )
            })
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
        let mut grid: Vec<String> = Vec::with_capacity(h as usize);
        for y in 0..h {
            let mut row = String::with_capacity(w as usize);
            for x in 0..w {
                let on = img.get_pixel(x, y).0[3] >= alpha_threshold;
                if on {
                    opaque += 1;
                    let (xi, yi) = (x as i32, y as i32);
                    bbox = Some(match bbox {
                        Some([a, b, c, d]) => [a.min(xi), b.min(yi), c.max(xi), d.max(yi)],
                        None => [xi, yi, xi, yi],
                    });
                }
                row.push(if on { '#' } else { '.' });
            }
            grid.push(row);
        }
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
            bbox: [i32; 4],                                  // tight [x0,y0,x1,y1]
            area: u32,                                       // opaque pixel count
            sx: u64,                                         // Σx (for the centroid)
            sy: u64,                                         // Σy (for the centroid)
            colors: std::collections::HashMap<[u8; 4], u32>, // colour → count (dominant)
        }
        let mut comps: Vec<Comp> = Vec::new();
        for sy in 0..h {
            for sx in 0..w {
                let si = (sy * w + sx) as usize;
                if seen[si] || !member(sx, sy) {
                    continue;
                }
                // BFS the component.
                let mut stack = vec![(sx, sy)];
                seen[si] = true;
                let mut c = Comp {
                    bbox: [sx, sy, sx, sy],
                    area: 0,
                    sx: 0,
                    sy: 0,
                    colors: std::collections::HashMap::new(),
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
                    *c.colors.entry(p).or_insert(0) += 1;
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
                    .max_by_key(|(_, n)| **n)
                    .map(|(c, _)| *c)
                    .unwrap_or([0, 0, 0, 0]);
                json!({
                    "bbox": c.bbox,
                    "centroid": [
                        (c.sx / c.area as u64) as i32,
                        (c.sy / c.area as u64) as i32,
                    ],
                    "area": c.area,
                    "dominant": format!("#{:02x}{:02x}{:02x}", dom[0], dom[1], dom[2]),
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
                            luma_sum += crate::raster::luma(p) as u64;
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

    /// Render a frame in an analysis colour space to a PNG you can SEE: grayscale
    /// (luma), `bands` (posterised luma), or the saturation/hue HSL channel as
    /// grey. Same output shape as doc_render; when `report`, adds value stats over
    /// the opaque pixels (min/max/mean grey, contrast, per-band coverage).
    #[allow(clippy::too_many_arguments)]
    pub fn doc_render_value(
        &self,
        id: &str,
        frame: usize,
        mode: &str,
        bands: u32,
        scale: u32,
        out_path: Option<&str>,
        report: bool,
    ) -> Result<Value, String> {
        let (dir, doc) = self.open(id)?;
        let img = doc.value_image(frame, mode, bands)?;
        let out = match out_path {
            Some(p) => PathBuf::from(p),
            None => dir.join(format!("value_{}_f{}.png", mode, frame)),
        };
        if let Some(p) = out.parent() {
            let _ = fs::create_dir_all(p);
        }
        // Save at scale (nearest) so the preview matches doc_render's behaviour.
        let sc = scale.max(1);
        let saved = if sc > 1 {
            image::imageops::resize(
                &img,
                img.width() * sc,
                img.height() * sc,
                image::imageops::FilterType::Nearest,
            )
        } else {
            img.clone()
        };
        let (w, h) = (saved.width(), saved.height());
        saved.save(&out).map_err(|e| e.to_string())?;
        let mut res = json!({"path": out.to_string_lossy(), "size": [w, h], "frame": frame});
        if report {
            // Stats over the grey value of opaque pixels (analysis channel).
            let nb = bands.max(1) as usize;
            let mut counts = vec![0u64; nb];
            let (mut min, mut max, mut sum, mut n) = (255u8, 0u8, 0u64, 0u64);
            for p in img.pixels() {
                if p.0[3] == 0 {
                    continue;
                }
                let v = p.0[0];
                min = min.min(v);
                max = max.max(v);
                sum += v as u64;
                n += 1;
                let b = (v as usize * nb / 256).min(nb - 1);
                counts[b] += 1;
            }
            if n == 0 {
                res["report"] = json!({
                    "min": Value::Null, "max": Value::Null, "mean": Value::Null,
                    "contrast": 0.0, "band_pcts": Vec::<f64>::new(),
                });
            } else {
                let band_pcts: Vec<f64> = counts
                    .iter()
                    .map(|c| (*c as f64 / n as f64 * 1000.0).round() / 1000.0)
                    .collect();
                res["report"] = json!({
                    "min": min,
                    "max": max,
                    "mean": (sum as f64 / n as f64).round() as u32,
                    "contrast": ((max - min) as f64 / 255.0 * 1000.0).round() / 1000.0,
                    "band_pcts": band_pcts,
                });
            }
        }
        Ok(res)
    }

    /// WCAG contrast check in one of three modes. `region`: mean colour inside vs
    /// a 4px surrounding band. `palette`: every pair of the frame's distinct
    /// opaque colours (capped 16). `one-bit`: threshold luma to a pure B/W PNG and
    /// report black/white coverage. `pass` = ratio ≥ `min_ratio`.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_contrast_check(
        &self,
        id: &str,
        frame: usize,
        mode: &str,
        region: Option<(i32, i32, i32, i32)>,
        min_ratio: f32,
        threshold: u8,
        out_path: Option<&str>,
    ) -> Result<Value, String> {
        let (dir, doc) = self.open(id)?;
        let img = doc.analysis_image(None, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let round2 = |v: f32| ((v * 100.0).round() / 100.0) as f64;
        match mode {
            "region" => {
                let (rx0, ry0, rx1, ry1) =
                    region.ok_or("region mode needs `region` [x0,y0,x1,y1]")?;
                let (x0, x1) = (rx0.min(rx1).max(0), rx0.max(rx1).min(w - 1));
                let (y0, y1) = (ry0.min(ry1).max(0), ry0.max(ry1).min(h - 1));
                if x0 > x1 || y0 > y1 {
                    return Err("region is empty after clamping to the canvas".into());
                }
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
                let ratio = crate::raster::wcag_ratio(a, b);
                Ok(json!({
                    "mode": "region",
                    "inside": format!("#{:02x}{:02x}{:02x}", a[0], a[1], a[2]),
                    "surround": format!("#{:02x}{:02x}{:02x}", b[0], b[1], b[2]),
                    "ratio": round2(ratio),
                    "pass": ratio >= min_ratio,
                }))
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
                        let ratio = crate::raster::wcag_ratio(colors[i], colors[j]);
                        let pass = ratio >= min_ratio;
                        let hex = |c: [u8; 4]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
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
                Ok(json!({
                    "mode": "palette",
                    "colors": colors.len(),
                    "pairs": pairs,
                    "failures": failures,
                }))
            }
            "one-bit" => {
                use image::{Rgba, RgbaImage};
                let out = match out_path {
                    Some(p) => PathBuf::from(p),
                    None => dir.join(format!("onebit_f{}.png", frame)),
                };
                if let Some(p) = out.parent() {
                    let _ = fs::create_dir_all(p);
                }
                let mut bw = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
                let (mut black, mut white) = (0u64, 0u64);
                for (x, y, p) in img.enumerate_pixels() {
                    if p.0[3] == 0 {
                        continue;
                    }
                    if crate::raster::luma(p.0) >= threshold {
                        bw.put_pixel(x, y, Rgba([255, 255, 255, 255]));
                        white += 1;
                    } else {
                        bw.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                        black += 1;
                    }
                }
                bw.save(&out).map_err(|e| e.to_string())?;
                let total = (black + white).max(1) as f64;
                Ok(json!({
                    "mode": "one-bit",
                    "path": out.to_string_lossy(),
                    "threshold": threshold,
                    "black_pct": (black as f64 / total * 1000.0).round() / 1000.0,
                    "white_pct": (white as f64 / total * 1000.0).round() / 1000.0,
                }))
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
        for f in frames {
            let img = doc.analysis_image(layer, f)?;
            let (x0, y0, x1, y1) = match region {
                Some((a, b, c, d)) => (
                    a.min(c).max(0),
                    b.min(d).max(0),
                    a.max(c).min(cw - 1),
                    b.max(d).min(ch - 1),
                ),
                None => (0, 0, cw - 1, ch - 1),
            };
            if x0 > x1 || y0 > y1 {
                return Err("region is empty after clamping to the canvas".into());
            }
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
        let hex = |c: [u8; 4]| format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]);
        let count = entries.len();
        let truncated = count > 256;
        let listed: Vec<&([u8; 4], u64)> = entries.iter().take(256).collect();
        let mut off_palette_count = 0u32;
        let colors: Vec<Value> = listed
            .iter()
            .map(|(c, n)| {
                let inp = if has_palette {
                    let v = in_pal(*c);
                    if !v {
                        off_palette_count += 1;
                    }
                    json!(v)
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
        Ok(json!({
            "count": count,
            "colors": colors,
            "off_palette_count": if has_palette { json!(off_palette_count) } else { Value::Null },
            "near_dupes": near_dupes,
            "truncated": truncated,
        }))
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
            .map(|c| crate::raster::luma(*c) as i32)
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
        let hues: Vec<f32> = ramp.iter().map(|c| crate::raster::hue_deg(*c)).collect();
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
        let sat: Vec<f32> = ramp.iter().map(|c| crate::raster::saturation(*c)).collect();
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
}

#[cfg(test)]
mod tests {
    use super::Studio;
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
    fn render_value_grayscale_and_report() {
        let s = studio("renderval");
        s.doc_create("d", 4, 4).unwrap();
        // one black-ish and one white pixel; rest transparent
        s.doc_pencil("d", 0, 0, vec![(0, 0)], [0, 0, 0, 255], 1)
            .unwrap();
        s.doc_pencil("d", 0, 0, vec![(1, 0)], [255, 255, 255, 255], 1)
            .unwrap();
        let out = s.docs_dir.join("val.png");
        let r = s
            .doc_render_value("d", 0, "grayscale", 4, 1, out.to_str(), true)
            .unwrap();
        assert!(out.exists());
        assert_eq!(r["size"], json!([4, 4])); // scale 1 keeps native size
        let rep = &r["report"];
        assert_eq!(rep["min"], json!(0)); // black luma
        assert_eq!(rep["max"], json!(255)); // white luma
        assert_eq!(rep["mean"], json!(128)); // (0+255)/2 rounded
        assert_eq!(rep["contrast"], json!(1.0)); // full value range
                                                 // unknown mode is an actionable error
        assert!(s
            .doc_render_value("d", 0, "bogus", 4, 1, None, false)
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
            .unwrap();
        assert_eq!(region["pass"], json!(true));
        assert!(region["ratio"].as_f64().unwrap() > 10.0); // black/white ≈ 21
                                                           // region mode without a region errors
        assert!(s
            .doc_contrast_check("d", 0, "region", None, 1.5, 128, None)
            .is_err());
        // palette mode: two distinct colours, one pair
        let pal = s
            .doc_contrast_check("d", 0, "palette", None, 1.5, 128, None)
            .unwrap();
        assert_eq!(pal["colors"], json!(2));
        assert_eq!(pal["pairs"].as_array().unwrap().len(), 1);
        // one-bit renders a B/W png and splits coverage
        let out = s.docs_dir.join("onebit.png");
        let ob = s
            .doc_contrast_check("d", 0, "one-bit", None, 1.5, 128, out.to_str())
            .unwrap();
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
}
