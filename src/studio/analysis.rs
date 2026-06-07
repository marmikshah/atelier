//! Read-only canvas analysis — the agent's other eye. These readers never
//! mutate the document (and ignore any active selection); they describe what is
//! already on the canvas: a text-grid dump, a silhouette report, connected
//! components, and a coarse coverage heatmap.

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
}
