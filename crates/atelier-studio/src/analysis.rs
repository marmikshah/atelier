//! Read-only canvas analysis — the agent's other eye. These readers never
//! mutate the document (and ignore any active selection); they describe what is
//! already on the canvas: a text-grid dump, a silhouette report, and
//! connected components.

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

    /// Indexed read of one cel: every pixel as its palette index, row-major —
    /// the inverse of `doc_paint_grid` with a palette-index legend, for
    /// consumers that think in indices rather than colours. Transparent
    /// pixels are `null` (transparency is not swatch 0 — swatch 0 is a real
    /// paintable colour; see `Document::indexed_raster`). An opaque pixel
    /// with no exact swatch match is an error, never a silent nearest-colour
    /// guess: snap first (`doc_palette op=snap`).
    pub fn doc_indexed_raster(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let r = doc.indexed_raster(layer, frame)?;
        Ok(json!({
            "doc_id": id,
            "layer": layer,
            "frame": frame,
            "width": r.width,
            "height": r.height,
            "palette": r.palette,
            "indices": r.indices,
        }))
    }

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
        let (cw, ch) = (doc.meta().w as i32, doc.meta().h as i32);
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

    // -- value & colour feedback (read-only analysis) -----------------------

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
            None => (0..doc.meta().frames.len()).collect(),
        };
        let (cw, ch) = (doc.meta().w as i32, doc.meta().h as i32);
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
        let has_palette = !doc.meta().palette.is_empty();
        let in_pal = |c: [u8; 4]| -> bool { doc.meta().palette.contains(&c) };
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
        let (cw, ch) = (doc.meta().w as i32, doc.meta().h as i32);
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
            // scale_nn clamps the caller's scale (1..=16) — this resize used to
            // take it raw: `cw * sc` could overflow u32 / allocate absurdly.
            let img = crate::scale_nn(&img, scale);
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
                        .meta()
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
                let (cw, ch) = (doc.meta().w as i32, doc.meta().h as i32);
                let (added, removed, recolored, bbox, ia, _ib) =
                    doc.frame_diff_region(last, first, layer, (0, 0, cw - 1, ch - 1))?;
                let changed = added + removed + recolored;
                // Denominator: opaque pixels of the played last frame (motion
                // base), counted from the flatten the diff already produced.
                let opaque = ia.pixels().filter(|p| p.0[3] > 0).count().max(1) as u64;
                let seam_score = (changed as f64 / opaque as f64 * 1000.0).round() / 1000.0;
                // Calibrate against the loop's own motion: whole-body animation
                // repaints most of the sprite EVERY step, so the absolute ratio
                // reads catastrophic even when the wrap is exactly as busy as
                // any mid-loop step. The honest question is "does the wrap
                // change more than a typical step?", so measure that.
                let mut steps: Vec<u32> = Vec::new();
                for w in seq.windows(2) {
                    let (ad, rm, rc, _, _, _) =
                        doc.frame_diff_region(w[0], w[1], layer, (0, 0, cw - 1, ch - 1))?;
                    steps.push(ad + rm + rc);
                }
                steps.sort_unstable();
                let typical = steps.get(steps.len() / 2).copied().unwrap_or(0);
                let wrap_vs_typical = (typical > 0)
                    .then(|| ((changed as f64 / typical as f64) * 100.0).round() / 100.0);
                let mut out = json!({
                    "seam_score": seam_score,
                    "changed": changed,
                    "added": added,
                    "removed": removed,
                    // WHERE the loop pops — fix this area, not the whole frame.
                    "change_bbox": bbox.map(|b| json!(b)).unwrap_or(Value::Null),
                    "frames": [last, first],
                    // Median changed-pixels of the loop's own adjacent steps.
                    "typical_step_changed": typical,
                    "wrap_vs_typical": wrap_vs_typical,
                });
                if let Some(r) = wrap_vs_typical {
                    if r <= 1.25 {
                        out["note"] = json!(
                            "the wrap changes about as much as a typical mid-loop step — for \
                             whole-body motion that is a healthy loop, not a pop, whatever \
                             seam_score says"
                        );
                    } else if r >= 2.0 {
                        out["note"] = json!(
                            "the wrap changes far more than a typical step — likely a visible \
                             pop; fix inside change_bbox"
                        );
                    }
                }
                Ok(out)
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
                    .map(|&f| doc.meta().frames[f].duration_ms)
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

/// The per-form lighting audit behind `critique`, factored out so it can be unit-tested without a
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
            // Hard floor under the caller's min_area: the least-squares plane
            // fit below needs more points than unknowns to mean anything.
            const MIN_FIT_AREA: u32 = 4;
            if area < min_area.max(MIN_FIT_AREA) {
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
            // Verdict thresholds. The R² gap (0.35..0.4) is hysteresis: a form
            // in between is neither confidently directional nor eligible for
            // the pillow call, so it lands on "flat" instead of flapping.
            /// Plane fit explaining at least this much lightness variance = one light.
            const DIRECTIONAL_R2: f64 = 0.4;
            /// Below this the fit is weak enough for a pillow verdict to stand.
            const PILLOW_MAX_R2: f64 = 0.35;
            /// Centre-distance correlation above this reads as pillow shading.
            const PILLOW_CORR: f64 = 0.5;
            let directional = plane_r2 >= DIRECTIONAL_R2 && mag > 1e-4;
            let is_pillow = pillow_corr > PILLOW_CORR && plane_r2 < PILLOW_MAX_R2;
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
    /// Azimuth spread beyond this many degrees = the forms disagree on the light.
    const LIGHT_SPREAD_DEG: f64 = 45.0;
    let inconsistent = azimuths.len() >= 2 && spread.map(|s| s > LIGHT_SPREAD_DEG).unwrap_or(false);
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

    /// Single draw-op shorthand: `params` is the op's JSON object (as `json!`).
    fn draw(s: &Studio, id: &str, frame: usize, op: &str, params: Value) -> Value {
        s.doc_draw(id, 0, frame, op, params.as_object().unwrap().clone())
            .unwrap()
    }

    #[test]
    fn dump_region_symbol_and_hex() {
        let s = studio("dump");
        s.doc_create("d", 4, 4).unwrap();
        // two distinct opaque pixels, rest transparent
        draw(
            &s,
            "d",
            0,
            "pencil",
            json!({"points": [[0, 0]], "color": [10, 20, 30, 255]}),
        );
        draw(
            &s,
            "d",
            0,
            "pencil",
            json!({"points": [[1, 0]], "color": [40, 50, 60, 255]}),
        );
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
        draw(
            &s,
            "d",
            0,
            "rect",
            json!({"x0": 1, "y0": 1, "x1": 2, "y1": 2, "color": [9, 9, 9, 255], "fill": true}),
        );
        let r = s.doc_silhouette("d", 0, None, 1).unwrap();
        assert_eq!(r["bbox"], json!([1, 1, 2, 2])); // 2x2 block
        assert_eq!(r["fill_ratio"], json!(0.25)); // 4 of 16 opaque
        assert_eq!(r["grid"][0], "...."); // empty top row
        assert_eq!(r["grid"][1], ".##."); // the block's first row
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
        draw(
            &s,
            "d",
            0,
            "rect",
            json!({"x0": 0, "y0": 0, "x1": 2, "y1": 2, "color": [255, 0, 0, 255], "fill": true}),
        );
        draw(
            &s,
            "d",
            0,
            "pencil",
            json!({"points": [[7, 7]], "color": [0, 255, 0, 255]}),
        );
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
    fn look_grayscale_writes_file_and_reports() {
        let s = studio("renderval");
        s.doc_create("d", 4, 4).unwrap();
        // one black-ish and one white pixel; rest transparent
        draw(
            &s,
            "d",
            0,
            "pencil",
            json!({"points": [[0, 0]], "color": [0, 0, 0, 255]}),
        );
        draw(
            &s,
            "d",
            0,
            "pencil",
            json!({"points": [[1, 0]], "color": [255, 255, 255, 255]}),
        );
        let out = s.docs_dir.join("val.png");
        let (png, r) = s
            .look(
                "d",
                0,
                &crate::LookOptions {
                    scale: Some(1),
                    mode: "grayscale".into(),
                    out_path: out.to_str().map(|s| s.to_string()),
                    ..Default::default()
                },
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
                &crate::LookOptions {
                    scale: Some(1),
                    mode: "bogus".into(),
                    ..Default::default()
                }
            )
            .is_err());
    }

    #[test]
    fn palette_report_counts_and_in_palette() {
        let s = studio("palrep");
        s.doc_create("d", 4, 4).unwrap();
        s.doc_set_palette("d", vec![[255, 0, 0, 255]]).unwrap();
        // 3 red (in palette) + 1 near-red green-stray (off palette)
        draw(
            &s,
            "d",
            0,
            "rect",
            json!({"x0": 0, "y0": 0, "x1": 2, "y1": 0, "color": [255, 0, 0, 255], "fill": true}),
        );
        draw(
            &s,
            "d",
            0,
            "pencil",
            json!({"points": [[0, 1]], "color": [250, 4, 0, 255]}),
        );
        let r = s.doc_palette_report("d", Some(0), None, None, 8).unwrap();
        assert_eq!(r["count"], 2);
        assert_eq!(r["colors"][0]["hex"], "#ff0000ff"); // most-used first
        assert_eq!(r["colors"][0]["in_palette"], json!(true));
        assert_eq!(r["off_palette_count"], json!(1)); // the near-red stray
                                                      // the two reds are within dist 8 → flagged as near-dupes
        assert_eq!(r["near_dupes"].as_array().unwrap().len(), 1);
        // no-palette doc → in_palette null
        s.doc_create("e", 2, 2).unwrap();
        draw(
            &s,
            "e",
            0,
            "pencil",
            json!({"points": [[0, 0]], "color": [1, 2, 3, 255]}),
        );
        let r2 = s.doc_palette_report("e", Some(0), None, None, 8).unwrap();
        assert_eq!(r2["colors"][0]["in_palette"], Value::Null);
        assert_eq!(r2["off_palette_count"], Value::Null);
    }

    #[test]
    fn frame_diff_classifies_changes_and_grids() {
        let s = studio("framediff");
        s.doc_create("d", 4, 4).unwrap();
        s.doc_add_frame("d", 100, Some(0), 1).unwrap(); // frame 1 copies frame 0
                                                        // frame 0: a red pixel at (0,0); frame 1: move it and recolour (1,1).
        draw(
            &s,
            "d",
            0,
            "pencil",
            json!({"points": [[0, 0]], "color": [255, 0, 0, 255]}),
        );
        draw(
            &s,
            "d",
            1,
            "pencil",
            json!({"points": [[1, 1]], "color": [0, 255, 0, 255]}),
        );
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
    fn seam_is_calibrated_against_the_loop_own_motion() {
        // Whole-body motion repaints most pixels EVERY step; the wrap being as
        // busy as a mid-loop step must read as healthy, not a pop.
        let s = studio("seamcal");
        s.doc_create("d", 8, 8).unwrap();
        for f in 0..4 {
            if f > 0 {
                s.doc_add_frame("d", 100, None, 1).unwrap();
            }
            // Every frame repaints a different column: big adjacent diffs.
            draw(
                &s,
                "d",
                f,
                "rect",
                json!({"x0": (f*2) as i32, "y0": 0, "x1": (f*2+1) as i32, "y1": 7,
                       "color": [200, 40, 40, 255], "fill": true}),
            );
        }
        let seam = s.doc_anim_audit("d", None, None, "seam", None).unwrap();
        // Absolute ratio is high (everything moved)…
        assert!(seam["seam_score"].as_f64().unwrap() > 0.5);
        // …but calibration says the wrap is a normal step.
        let r = seam["wrap_vs_typical"].as_f64().unwrap();
        assert!(r <= 1.25, "wrap ~ typical step, got {r}");
        assert!(
            seam["note"].as_str().unwrap().contains("healthy loop"),
            "note: {}",
            seam["note"]
        );
        assert!(seam["typical_step_changed"].as_u64().unwrap() > 0);
    }

    #[test]
    fn anim_audit_seam_and_spacing() {
        let s = studio("animaudit");
        s.doc_create("d", 8, 8).unwrap();
        // 3 frames: a 2x2 block stepping right by 2 each frame (even spacing).
        draw(
            &s,
            "d",
            0,
            "rect",
            json!({"x0": 0, "y0": 0, "x1": 1, "y1": 1, "color": [9, 9, 9, 255], "fill": true}),
        );
        s.doc_add_frame("d", 100, None, 1).unwrap();
        draw(
            &s,
            "d",
            1,
            "rect",
            json!({"x0": 2, "y0": 0, "x1": 3, "y1": 1, "color": [9, 9, 9, 255], "fill": true}),
        );
        s.doc_add_frame("d", 100, None, 1).unwrap();
        draw(
            &s,
            "d",
            2,
            "rect",
            json!({"x0": 4, "y0": 0, "x1": 5, "y1": 1, "color": [9, 9, 9, 255], "fill": true}),
        );
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
}
