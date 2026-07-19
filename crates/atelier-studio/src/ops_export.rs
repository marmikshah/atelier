//! File exports: spritesheets, animations, tilesets, and the library-wide
//! packers (`all` / `atlas`). Every entry point writes caller-supplied paths,
//! so each funnels through the same parent-dir guard before touching disk.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use super::{export_scale, Studio, DEFAULT_EXPORT_SCALE};

/// Create the parent directory of an export target (one guard, not one per
/// entry point). An empty/absent parent is a no-op.
fn ensure_parent(out_path: &str) -> Result<(), String> {
    if let Some(p) = Path::new(out_path).parent() {
        fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
    }
    Ok(())
}

/// Escape the four XML metacharacters double-quoted attribute values need
/// (apostrophes are legal there) so values stay well-formed.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl Studio {
    /// Export every document as a spritesheet PNG (+ JSON meta) into a flat
    /// target dir for a game's assets/.
    pub fn export_all(&self, target_dir: &str, scale: u32) -> Result<Value, String> {
        let target = Path::new(target_dir);
        fs::create_dir_all(target).map_err(|e| e.to_string())?;
        let mut exported = Vec::new();
        for id in self.doc_ids() {
            let (_dir, doc) = self.open(&id)?;
            let out = target.join(format!("{}.png", id));
            doc.export_sheet(&out, export_scale(scale))?;
            exported.push(out.to_string_lossy().to_string());
        }
        Ok(json!({"target": target.to_string_lossy(), "exported": exported}))
    }

    /// Pack every frame of every document into ONE atlas PNG (a simple shelf
    /// layout: rows of frames, wrapping at `max_width`) plus a master JSON map
    /// of `{doc_id, frame, rect:[x,y,w,h], duration_ms}` so an engine can
    /// slice the whole game's sprites from a single texture. Frames are emitted
    /// at native size × `scale`.
    pub fn export_atlas(
        &self,
        out_path: &str,
        scale: u32,
        max_width: u32,
    ) -> Result<Value, String> {
        use image::{Rgba, RgbaImage};
        let scale = export_scale(scale);
        let ids = self.doc_ids();
        if ids.is_empty() {
            return Err("no documents to pack".into());
        }
        // Gather every frame image first (so we can size the atlas).
        struct Item {
            doc: String,      // owning document id
            frame: usize,     // frame index within that document
            img: RgbaImage,   // flattened (and scaled) frame pixels
            duration_ms: u32, // frame duration, carried into the map
        }
        let mut items: Vec<Item> = Vec::new();
        for id in &ids {
            let (_dir, doc) = self.open(id)?;
            for f in 0..doc.meta().frames.len() {
                let mut img = doc.flatten(f);
                if scale > 1 {
                    img = image::imageops::resize(
                        &img,
                        doc.meta().w * scale,
                        doc.meta().h * scale,
                        image::imageops::FilterType::Nearest,
                    );
                }
                items.push(Item {
                    doc: id.clone(),
                    frame: f,
                    img,
                    duration_ms: doc.meta().frames[f].duration_ms,
                });
            }
        }
        // Shelf pack: lay frames left→right, wrap when the row exceeds max_width.
        let min_w = items.iter().map(|i| i.img.width()).max().unwrap_or(1);
        let max_width = max_width.max(min_w);
        let (pad, mut x, mut y, mut row_h, mut atlas_w) = (1u32, 0u32, 0u32, 0u32, 0u32);
        let mut placed: Vec<(usize, u32, u32)> = Vec::new(); // (item idx, x, y)
        for (idx, it) in items.iter().enumerate() {
            let (iw, ih) = (it.img.width(), it.img.height());
            if x > 0 && x + iw > max_width {
                x = 0;
                y += row_h + pad;
                row_h = 0;
            }
            placed.push((idx, x, y));
            x += iw + pad;
            atlas_w = atlas_w.max(x.saturating_sub(pad));
            row_h = row_h.max(ih);
        }
        let atlas_h = y + row_h;
        let mut atlas = RgbaImage::from_pixel(atlas_w.max(1), atlas_h.max(1), Rgba([0, 0, 0, 0]));
        let mut frames_meta: Vec<Value> = Vec::new();
        for (idx, px, py) in placed {
            let it = &items[idx];
            image::imageops::replace(&mut atlas, &it.img, px as i64, py as i64);
            frames_meta.push(json!({
                "doc": it.doc, "frame": it.frame,
                "rect": [px, py, it.img.width(), it.img.height()],
                "duration_ms": it.duration_ms,
            }));
        }
        ensure_parent(out_path)?;
        atlas.save(out_path).map_err(|e| e.to_string())?;
        let meta = json!({
            "path": out_path, "atlas_w": atlas_w, "atlas_h": atlas_h,
            "count": frames_meta.len(), "frames": frames_meta,
        });
        let mp = Path::new(out_path).with_extension("json");
        std::fs::write(
            &mp,
            serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(meta)
    }

    pub fn doc_export_sheet(&self, id: &str, out_path: &str, scale: u32) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        ensure_parent(out_path)?;
        doc.export_sheet(Path::new(out_path), export_scale(scale))
    }

    /// One-tool dispatch over the per-document file exports — `op`: `sheet` |
    /// `anim` | `tileset`. Shared `out_path`/`scale`; op-specific params come
    /// flattened (sheet: `meta`; anim: `format`,`tag`; tileset: `tile_w`,
    /// `tile_h`). The library-wide exports are the sibling [`Self::export_all`]
    /// and [`Self::export_atlas`], which the MCP layer fuses onto the same tool
    /// as `doc_export op=all|atlas`.
    pub fn doc_export(
        &self,
        id: &str,
        op: &str,
        out_path: &str,
        scale: Option<u32>,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        let geti = |k: &str| params.get(k).and_then(|v| v.as_u64()).map(|n| n as u32);
        let gets = |k: &str| params.get(k).and_then(|v| v.as_str());
        match op {
            "sheet" => match gets("meta").unwrap_or("atelier") {
                "atelier" => self.doc_export_sheet(id, out_path, scale.unwrap_or(DEFAULT_EXPORT_SCALE)),
                "standard" => {
                    let (_dir, doc) = self.open(id)?;
                    ensure_parent(out_path)?;
                    doc.export_sheet_std(Path::new(out_path), export_scale(scale.unwrap_or(DEFAULT_EXPORT_SCALE)))
                }
                other => Err(format!(
                    "doc_export op=sheet: unknown meta '{other}' — use atelier|standard"
                )),
            },
            "anim" => match gets("format").unwrap_or("gif") {
                "apng" => self.doc_export_apng(id, out_path, scale.unwrap_or(DEFAULT_EXPORT_SCALE), gets("tag")),
                "gif" => self.doc_export_gif(id, out_path, scale.unwrap_or(DEFAULT_EXPORT_SCALE), gets("tag")),
                other => Err(format!(
                    "doc_export op=anim: unknown format '{other}' — use gif|apng"
                )),
            },
            "tileset" => {
                let tw = geti("tile_w").ok_or("doc_export op=tileset needs tile_w")?;
                let th = geti("tile_h").ok_or("doc_export op=tileset needs tile_h")?;
                self.export_tileset(id, tw, th, scale.unwrap_or(1), out_path)
            }
            other => Err(format!(
                "doc_export: unknown op '{other}' — use sheet|anim|tileset (all|atlas are the library-wide ops)"
            )),
        }
    }

    pub fn doc_export_gif(
        &self,
        id: &str,
        out_path: &str,
        scale: u32,
        tag: Option<&str>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        ensure_parent(out_path)?;
        let frames = doc.export_gif(Path::new(out_path), export_scale(scale), tag)?;
        Ok(json!({"path": out_path, "frames": frames, "tag": tag}))
    }

    pub fn doc_export_apng(
        &self,
        id: &str,
        out_path: &str,
        scale: u32,
        tag: Option<&str>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        ensure_parent(out_path)?;
        let frames = doc.export_apng(Path::new(out_path), export_scale(scale), tag)?;
        Ok(json!({"path": out_path, "frames": frames, "tag": tag}))
    }

    /// Slice frame 0 (flattened, nearest-scaled) into a `tile_w`×`tile_h` grid and
    /// write engine-ready tileset metadata: the PNG plus TWO sidecars — `<name>.tsx`
    /// (Tiled XML) and `<name>.json` (the same fields as JSON). The canvas must be
    /// exactly divisible by the tile size.
    pub fn export_tileset(
        &self,
        id: &str,
        tile_w: u32,
        tile_h: u32,
        scale: u32,
        out_path: &str,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let (tile_w, tile_h, scale) = (tile_w.max(1), tile_h.max(1), export_scale(scale));
        let (cw, ch) = (doc.meta().w, doc.meta().h);
        if cw % tile_w != 0 || ch % tile_h != 0 {
            return Err(format!(
                "canvas {}x{} not divisible by tile {}x{}",
                cw, ch, tile_w, tile_h
            ));
        }
        let (columns, rows) = (cw / tile_w, ch / tile_h);
        let tilecount = columns * rows;
        let (out_w, out_h) = (cw * scale, ch * scale);
        let mut img = doc.flatten(0);
        if scale > 1 {
            img = image::imageops::resize(&img, out_w, out_h, image::imageops::FilterType::Nearest);
        }
        let out = Path::new(out_path);
        ensure_parent(out_path)?;
        img.save(out).map_err(|e| e.to_string())?;
        // Scaled tile size — what an engine slices against the emitted PNG.
        let (stw, sth) = (tile_w * scale, tile_h * scale);
        let source = out
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| out_path.to_string());
        let tsx = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <tileset version=\"1.10\" name=\"{name}\" tilewidth=\"{tw}\" tileheight=\"{th}\" \
             tilecount=\"{count}\" columns=\"{cols}\">\n\
             \x20<image source=\"{src}\" width=\"{iw}\" height=\"{ih}\"/>\n\
             </tileset>\n",
            name = id,
            tw = stw,
            th = sth,
            count = tilecount,
            cols = columns,
            src = xml_escape(&source),
            iw = out_w,
            ih = out_h,
        );
        let tsx_path = out.with_extension("tsx");
        fs::write(&tsx_path, &tsx).map_err(|e| e.to_string())?;
        let meta = json!({
            "name": id, "image": source, "image_w": out_w, "image_h": out_h,
            "tilewidth": stw, "tileheight": sth,
            "tilecount": tilecount, "columns": columns, "rows": rows,
        });
        let json_path = out.with_extension("json");
        fs::write(
            &json_path,
            serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(json!({
            "path": out.to_string_lossy(), "tsx": tsx_path.to_string_lossy(),
            "json": json_path.to_string_lossy(),
            "tilecount": tilecount, "columns": columns, "rows": rows,
            "tilewidth": stw, "tileheight": sth,
        }))
    }
}
