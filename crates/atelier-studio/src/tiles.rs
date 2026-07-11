//! Tile-family generators: the deterministic 16-tile Wang corner set
//! (`wang_tiles`), the 47-tile blob autotile family (`autotile_set`), and
//! in-situ tilemap assembly from a terrain mask (`tilemap_assemble`), plus
//! the corner/edge predicates they share.

use std::fs;

use serde_json::{json, Value};

use super::Studio;
use atelier_core::document::Document;

impl Studio {
    /// Generate the deterministic 16-tile Wang/blob set from a terrain source: its
    /// frame 0 carries the INNER material on layer 0 and the OUTER material on
    /// layer 1 (top-left N×N of each layer is sampled). Output is a NEW document
    /// `<id>-wang`, canvas 4N×4N, laid out as a 4×4 grid of every corner
    /// combination (tile index = bits NE,SE,SW,NW). Each set corner bit fills a
    /// quarter-disc (radius N/2) at that tile corner with the inner material;
    /// adjacent set corners connect via the shared half-edge. Returns the new
    /// document's structure + id.
    pub fn wang_tiles(&self, id: &str, n: u32) -> Result<Value, String> {
        use image::{Rgba, RgbaImage};
        let (_dir, src) = self.open(id)?;
        if src.meta.layers.len() < 2 {
            return Err(
                "wang_tiles needs two layers: layer 0 = inner material, layer 1 = outer material"
                    .into(),
            );
        }
        let n = n.max(1);
        if src.meta.w < n || src.meta.h < n {
            return Err(format!(
                "source canvas {}x{} smaller than tile size {}",
                src.meta.w, src.meta.h, n
            ));
        }
        // Sample the top-left N×N of each layer's full-canvas image.
        let inner = src.analysis_image(Some(0), 0)?;
        let outer = src.analysis_image(Some(1), 0)?;
        let r = n as f32 / 2.0; // corner quarter-disc radius
                                // The four corners of a tile, in bit order NE,SE,SW,NW (bit 0 = NE).
        let corners: [(u32, u32); 4] = [
            (n, 0), // NE (top-right)
            (n, n), // SE (bottom-right)
            (0, n), // SW (bottom-left)
            (0, 0), // NW (top-left)
        ];
        let mut canvas = RgbaImage::from_pixel(4 * n, 4 * n, Rgba([0, 0, 0, 0]));
        for tile in 0..16u32 {
            let (gx, gy) = (tile % 4, tile / 4); // 4×4 grid placement
            let (ox, oy) = (gx * n, gy * n);
            for ty in 0..n {
                for tx in 0..n {
                    let inside = Self::wang_inside(tx, ty, n, r, &corners, tile);
                    let mat = if inside { &inner } else { &outer };
                    let p = *mat.get_pixel(tx, ty);
                    canvas.put_pixel(ox + tx, oy + ty, p);
                }
            }
        }
        // Materialise the new document and place the grid as its single cel.
        let new_id = self.unique_id(&format!("{}-wang", id));
        let dir = self.doc_dir(&new_id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut doc = Document::new(&format!("{}-wang", id), 4 * n, 4 * n);
        doc.set_cel(0, 0, 0, 0, canvas)?;
        doc.save(&dir)?;
        let mut out = doc.structure();
        out["id"] = json!(new_id);
        out["tile_size"] = json!(n);
        Ok(out)
    }

    /// True when pixel (tx,ty) inside an N×N tile is INNER material for `tile`'s
    /// corner bitmask: it lies within a set corner's quarter-disc (radius `r`), or
    /// in the half-edge rectangle joining two adjacent set corners. `corners` is
    /// the (corner_x, corner_y) of each bit in order NE,SE,SW,NW.
    fn wang_inside(tx: u32, ty: u32, n: u32, r: f32, corners: &[(u32, u32); 4], tile: u32) -> bool {
        let (px, py) = (tx as f32 + 0.5, ty as f32 + 0.5);
        // Quarter-disc per set corner.
        for (bit, &(cx, cy)) in corners.iter().enumerate() {
            if tile & (1 << bit) == 0 {
                continue;
            }
            let (dx, dy) = (px - cx as f32, py - cy as f32);
            if dx * dx + dy * dy <= r * r {
                return true;
            }
        }
        // Half-edge rectangles when both corners on an edge are set. Bits are
        // NE=0, SE=1, SW=2, NW=3; edges connect adjacent corners by filling the
        // half-depth band along their shared edge.
        let bit = |b: u32| tile & (1 << b) != 0;
        let half = n as f32 / 2.0;
        // Top edge: NW(3) + NE(0).
        if bit(3) && bit(0) && (py <= half) {
            return true;
        }
        // Right edge: NE(0) + SE(1).
        if bit(0) && bit(1) && (px >= half) {
            return true;
        }
        // Bottom edge: SE(1) + SW(2).
        if bit(1) && bit(2) && (py >= half) {
            return true;
        }
        // Left edge: SW(2) + NW(3).
        if bit(2) && bit(3) && (px <= half) {
            return true;
        }
        false
    }

    /// Generate the deterministic 47-tile BLOB autotile set (the full
    /// edge+corner bitmask family — the modern superset of the 16-corner Wang
    /// set). Source contract matches `wang_tiles`: frame 0, layer 0 = inner
    /// material, layer 1 = outer, top-left N×N sampled. Output is a NEW
    /// document `<id>-blob` laid out as a 7×7 grid of the 47 canonical
    /// neighbour masks (a corner bit only counts when both adjacent edges are
    /// set). Returns the new doc's structure plus `masks` — the canonical
    /// 8-bit neighbour mask per grid index (N=1 NE=2 E=4 SE=8 S=16 SW=32 W=64
    /// NW=128) — so an engine autotiler can map straight onto it.
    pub fn autotile_set(&self, id: &str, n: u32) -> Result<Value, String> {
        use image::{Rgba, RgbaImage};
        let (_dir, src) = self.open(id)?;
        if src.meta.layers.len() < 2 {
            return Err(
                "autotile_set needs two layers: layer 0 = inner material, layer 1 = outer material"
                    .into(),
            );
        }
        let n = n.max(2);
        if src.meta.w < n || src.meta.h < n {
            return Err(format!(
                "source canvas {}x{} smaller than tile size {}",
                src.meta.w, src.meta.h, n
            ));
        }
        let inner = src.analysis_image(Some(0), 0)?;
        let outer = src.analysis_image(Some(1), 0)?;
        let masks = Self::blob_masks();
        let mut canvas = RgbaImage::from_pixel(7 * n, 7 * n, Rgba([0, 0, 0, 0]));
        for (idx, &mask) in masks.iter().enumerate() {
            let (gx, gy) = ((idx as u32) % 7, (idx as u32) / 7);
            for ty in 0..n {
                for tx in 0..n {
                    let p = if Self::blob_inside(tx, ty, n, mask) {
                        *inner.get_pixel(tx, ty)
                    } else {
                        *outer.get_pixel(tx, ty)
                    };
                    canvas.put_pixel(gx * n + tx, gy * n + ty, p);
                }
            }
        }
        let new_id = self.unique_id(&format!("{}-blob", id));
        let dir = self.doc_dir(&new_id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut doc = Document::new(&format!("{}-blob", id), 7 * n, 7 * n);
        doc.set_cel(0, 0, 0, 0, canvas)?;
        doc.save(&dir)?;
        let mut out = doc.structure();
        out["id"] = json!(new_id);
        out["tile_size"] = json!(n);
        out["masks"] = json!(masks);
        Ok(out)
    }

    /// Assemble a TILEMAP from a terrain mask — the in-situ test of a tileset,
    /// and the only real one. `rows` is the map as strings (`#`/`1`/`x` =
    /// filled); each filled cell computes its 8-neighbour mask and renders
    /// directly from the same materials + blob rules as `autotile_set` (no
    /// intermediate sheet needed), so what you see IS what the autotile family
    /// produces. `outside` says how off-map reads: `filled` (terrain continues,
    /// default) or `empty` (map edges get borders). Output is a NEW document
    /// `<id>-map` the agent can doc_look / doc_export.
    pub fn tilemap_assemble(
        &self,
        id: &str,
        n: u32,
        rows: &[String],
        outside_filled: bool,
    ) -> Result<Value, String> {
        use image::{Rgba, RgbaImage};
        let (_dir, src) = self.open(id)?;
        if src.meta.layers.len() < 2 {
            return Err(
                "tilemap_assemble needs two layers: layer 0 = inner material, layer 1 = outer material"
                    .into(),
            );
        }
        let n = n.max(2);
        if src.meta.w < n || src.meta.h < n {
            return Err(format!(
                "source canvas {}x{} smaller than tile size {}",
                src.meta.w, src.meta.h, n
            ));
        }
        if rows.is_empty() || rows.iter().any(|r| r.is_empty()) {
            return Err("mask rows must be non-empty strings".into());
        }
        let h = rows.len() as i32;
        let w = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0) as i32;
        let grid: Vec<Vec<bool>> = rows
            .iter()
            .map(|r| {
                let mut v: Vec<bool> = r.chars().map(|c| matches!(c, '#' | '1' | 'x')).collect();
                v.resize(w as usize, false);
                v
            })
            .collect();
        let filled = |x: i32, y: i32| -> bool {
            if x < 0 || y < 0 || x >= w || y >= h {
                outside_filled
            } else {
                grid[y as usize][x as usize]
            }
        };
        let inner = src.analysis_image(Some(0), 0)?;
        let outer = src.analysis_image(Some(1), 0)?;
        let mut canvas = RgbaImage::from_pixel(w as u32 * n, h as u32 * n, Rgba([0, 0, 0, 0]));
        let mut cells = 0u32;
        for cy in 0..h {
            for cx in 0..w {
                if !filled(cx, cy) {
                    continue;
                }
                cells += 1;
                // 8-neighbour mask: N=1 NE=2 E=4 SE=8 S=16 SW=32 W=64 NW=128.
                let dirs = [
                    (0, -1),
                    (1, -1),
                    (1, 0),
                    (1, 1),
                    (0, 1),
                    (-1, 1),
                    (-1, 0),
                    (-1, -1),
                ];
                let mut mask = 0u8;
                for (bit, (dx, dy)) in dirs.iter().enumerate() {
                    if filled(cx + dx, cy + dy) {
                        mask |= 1 << bit;
                    }
                }
                for ty in 0..n {
                    for tx in 0..n {
                        let p = if Self::blob_inside(tx, ty, n, mask) {
                            *inner.get_pixel(tx, ty)
                        } else {
                            *outer.get_pixel(tx, ty)
                        };
                        canvas.put_pixel(cx as u32 * n + tx, cy as u32 * n + ty, p);
                    }
                }
            }
        }
        let new_id = self.unique_id(&format!("{}-map", id));
        let dir = self.doc_dir(&new_id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut doc = Document::new(&format!("{}-map", id), w as u32 * n, h as u32 * n);
        doc.set_cel(0, 0, 0, 0, canvas)?;
        doc.save(&dir)?;
        let mut out = doc.structure();
        out["id"] = json!(new_id);
        out["tile_size"] = json!(n);
        out["cells_filled"] = json!(cells);
        Ok(out)
    }

    /// The 47 canonical blob neighbour masks: every 8-bit mask with each corner
    /// bit zeroed unless BOTH its adjacent edge bits are set, deduplicated.
    /// Bit order: N=1 NE=2 E=4 SE=8 S=16 SW=32 W=64 NW=128.
    fn blob_masks() -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        for m in 0u16..256 {
            let m = m as u8;
            let e = |b: u8| m & b != 0;
            let mut c = m;
            // NE needs N+E, SE needs E+S, SW needs S+W, NW needs W+N.
            if !(e(1) && e(4)) {
                c &= !2;
            }
            if !(e(4) && e(16)) {
                c &= !8;
            }
            if !(e(16) && e(64)) {
                c &= !32;
            }
            if !(e(64) && e(1)) {
                c &= !128;
            }
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// True when pixel (tx,ty) of an N×N blob tile is INNER material for the
    /// canonical neighbour `mask`. Border band width = N/4: an empty edge
    /// neighbour paints its band outer; a filled-edges/empty-diagonal corner
    /// gets an outer notch — the full 47-appearance family from one predicate.
    fn blob_inside(tx: u32, ty: u32, n: u32, mask: u8) -> bool {
        let b = (n / 4).max(1);
        let e = |bit: u8| mask & bit != 0;
        let (left, right, top, bottom) = (tx < b, tx >= n - b, ty < b, ty >= n - b);
        // Edge bands toward empty neighbours.
        if (top && !e(1)) || (right && !e(4)) || (bottom && !e(16)) || (left && !e(64)) {
            return false;
        }
        // Inner-corner notches: both edges filled, diagonal empty.
        if top && right && e(1) && e(4) && !e(2) {
            return false;
        }
        if bottom && right && e(4) && e(16) && !e(8) {
            return false;
        }
        if bottom && left && e(16) && e(64) && !e(32) {
            return false;
        }
        if top && left && e(64) && e(1) && !e(128) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-tiles-{}", tag));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    // Read one pixel via the Document model; the wang assertions compare raw RGBA.
    fn px(s: &Studio, id: &str, layer: usize, frame: usize, x: i32, y: i32) -> [u8; 4] {
        let (_dir, doc) = s.open(id).unwrap();
        doc.get_pixel(layer, frame, x, y).unwrap()
    }

    fn terrain_source(s: &Studio, id: &str) {
        // Layer 0 = solid green inner, layer 1 = solid brown outer.
        s.doc_create(id, 8, 8).unwrap();
        s.doc_fill_cel(id, 0, 0, [60, 140, 60, 255]).unwrap();
        s.doc_add_layer(id, Some("outer".into()), 255, "normal".into())
            .unwrap();
        s.doc_fill_cel(id, 1, 0, [110, 80, 50, 255]).unwrap();
    }

    #[test]
    fn blob_masks_are_exactly_the_canonical_47() {
        let masks = Studio::blob_masks();
        assert_eq!(masks.len(), 47);
        // Every mask is canonical: corner bits only with both adjacent edges.
        for &m in &masks {
            let e = |b: u8| m & b != 0;
            assert!(!e(2) || (e(1) && e(4)), "NE needs N+E in {m:#010b}");
            assert!(!e(8) || (e(4) && e(16)), "SE needs E+S in {m:#010b}");
            assert!(!e(32) || (e(16) && e(64)), "SW needs S+W in {m:#010b}");
            assert!(!e(128) || (e(64) && e(1)), "NW needs W+N in {m:#010b}");
        }
    }

    #[test]
    fn autotile_set_builds_the_7x7_sheet() {
        let s = studio("blobset");
        terrain_source(&s, "terra");
        let r = s.autotile_set("terra", 8).unwrap();
        assert_eq!(r["w"], 56); // 7 × 8
        assert_eq!(r["h"], 56);
        assert_eq!(r["masks"].as_array().unwrap().len(), 47);
        // The all-neighbours tile (mask 255) is pure inner everywhere; find its
        // grid slot and probe its centre and corner.
        let masks = r["masks"].as_array().unwrap();
        let idx = masks.iter().position(|m| m == 255).unwrap() as u32;
        let (gx, gy) = (idx % 7 * 8, idx / 7 * 8);
        let id = r["id"].as_str().unwrap();
        for (px, py) in [(gx + 4, gy + 4), (gx, gy), (gx + 7, gy + 7)] {
            let p = s
                .doc_get_pixel(id, Some(0), 0, px as i32, py as i32)
                .unwrap();
            assert_eq!(p["rgba"], json!([60, 140, 60, 255]), "at {px},{py}");
        }
    }

    #[test]
    fn tilemap_assemble_renders_interior_and_edges() {
        let s = studio("blobmap");
        terrain_source(&s, "terra");
        // A 3×3 plus shape with empty outside: the centre cell has all four
        // edge neighbours, so its edge bands stay inner; the top cell's top
        // edge faces empty and must render outer.
        let rows = vec!["·#·".into(), "###".into(), "·#·".into()];
        let r = s.tilemap_assemble("terra", 8, &rows, false).unwrap();
        assert_eq!(r["w"], 24);
        assert_eq!(r["cells_filled"], 5);
        let id = r["id"].as_str().unwrap();
        let px = |x: i32, y: i32| s.doc_get_pixel(id, Some(0), 0, x, y).unwrap()["rgba"].clone();
        // Centre cell (8..16, 8..16): its top band (y=8) borders a filled cell → inner.
        assert_eq!(px(12, 8), json!([60, 140, 60, 255]));
        // Top cell's top edge (y=0) faces empty → outer band.
        assert_eq!(px(12, 0), json!([110, 80, 50, 255]));
        // Centre-cell corner notch: centre's NE diagonal is empty while N and E
        // are filled → the notch pixel at the cell's top-right goes outer.
        assert_eq!(px(15, 8), json!([110, 80, 50, 255]));
        // Empty cells stay transparent.
        assert_eq!(px(0, 0), json!([0, 0, 0, 0]));
    }

    #[test]
    fn wang_tiles_single_layer_errors_with_actionable_message() {
        let s = studio("wang-onelayer");
        s.doc_create("terrain", 8, 8).unwrap(); // only layer 0
        let err = s.wang_tiles("terrain", 8).unwrap_err();
        assert!(err.contains("needs two layers") && err.contains("layer 1 = outer material"));
    }

    #[test]
    fn wang_doc_is_4n_and_corner_tiles_are_pure() {
        let s = studio("wang");
        let n = 8u32;
        s.doc_create("terrain", n, n).unwrap();
        s.doc_add_layer("terrain", None, 255, "normal".into())
            .unwrap(); // layer 1 = outer
        let inner = [200, 50, 50, 255];
        let outer = [30, 60, 120, 255];
        s.doc_fill_cel("terrain", 0, 0, inner).unwrap(); // layer 0 inner
        s.doc_fill_cel("terrain", 1, 0, outer).unwrap(); // layer 1 outer
        let out = s.wang_tiles("terrain", n).unwrap();
        let wid = out["id"].as_str().unwrap().to_string();
        // The new doc is 4N×4N.
        assert_eq!(out["w"], 4 * n);
        assert_eq!(out["h"], 4 * n);
        // Tile 0 (no corner bits) sits at grid (0,0) and is all-outer.
        assert_eq!(px(&s, &wid, 0, 0, 0, 0), outer);
        assert_eq!(px(&s, &wid, 0, 0, (n - 1) as i32, (n - 1) as i32), outer);
        // Tile 15 (all corners) sits at grid (3,3) and is all-inner.
        let (b15x, b15y) = (3 * n, 3 * n);
        assert_eq!(px(&s, &wid, 0, 0, b15x as i32, b15y as i32), inner);
        assert_eq!(
            px(&s, &wid, 0, 0, (b15x + n - 1) as i32, (b15y + n - 1) as i32),
            inner
        );
        // Tile 1 = NE corner only (bit 0); grid (1,0). The top-right pixel is
        // inner (in the NE quarter-disc); the bottom-left pixel is outer.
        let (b1x, b1y) = (n, 0u32);
        assert_eq!(px(&s, &wid, 0, 0, (b1x + n - 1) as i32, b1y as i32), inner);
        assert_eq!(px(&s, &wid, 0, 0, b1x as i32, (b1y + n - 1) as i32), outer);
    }
}
