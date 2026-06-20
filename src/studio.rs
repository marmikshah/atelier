//! The document store: a flat library of editable pixel-art documents.
//!
//! State lives under ~/.atelier (override with ATELIER_HOME). Each document
//! is a directory `documents/<id>/` with a `doc.json` (structure + cel refs) and
//! one PNG per cel under `cels/`. There is no project/grouping layer — a document
//! is the unit, addressed by its `id` (a slug derived from its name).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::document::Document;

mod analysis;
mod craft;
mod reference;

fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

/// Escape the five XML metacharacters so attribute values stay well-formed.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A copied rectangular region: width, height, flat RGBA buffer.
type Clip = (u32, u32, Vec<u8>);

/// An active pixel selection: which document it belongs to, its dimensions, and
/// one `bool` per pixel (row-major). Painting ops confine to the `true` pixels.
#[derive(Clone)]
struct Selection {
    doc_id: String,
    w: u32,
    h: u32,
    mask: Vec<bool>,
}

/// A by-colour selection request: which cel to read, an explicit target colour
/// or a sample point to read it from, and the channel-distance tolerance.
pub struct ColorSelect {
    pub layer: usize,
    pub frame: usize,
    pub color: Option<[u8; 4]>,
    pub sample: Option<(i32, i32)>,
    pub tol: i32,
}

#[derive(Clone)]
pub struct Studio {
    docs_dir: PathBuf,
    /// Cross-cel / cross-document clipboard for copy/cut → paste. Lives for the
    /// process; one shared studio means one shared clipboard across sessions.
    clipboard: Option<Clip>,
    /// Active selection mask (at most one), set by `doc_select`; painting ops
    /// confine to it. Process-lived, like the clipboard.
    selection: Option<Selection>,
}

impl Studio {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Studio {
        let home = std::env::var("ATELIER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".atelier"));
        let docs_dir = home.join("documents");
        let _ = fs::create_dir_all(&docs_dir);
        Studio {
            docs_dir,
            clipboard: None,
            selection: None,
        }
    }

    /// Test-only: build a studio rooted at an explicit directory (avoids the
    /// process-global ATELIER_HOME env var, so tests stay parallel-safe).
    #[cfg(test)]
    pub(crate) fn with_docs_dir(docs_dir: PathBuf) -> Studio {
        let _ = fs::create_dir_all(&docs_dir);
        Studio {
            docs_dir,
            clipboard: None,
            selection: None,
        }
    }

    fn doc_dir(&self, id: &str) -> PathBuf {
        self.docs_dir.join(id)
    }

    /// Stored ids are always slugs (`doc_create` slugifies the name). Reject
    /// anything else before it reaches the filesystem — ids arrive untrusted
    /// over MCP, and an id like `../x` would otherwise escape the store.
    fn valid_id(id: &str) -> bool {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    fn exists(&self, id: &str) -> bool {
        self.doc_dir(id).join("doc.json").exists()
    }

    /// All document ids on disk (directories with a doc.json), sorted.
    fn doc_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.docs_dir) {
            for e in rd.flatten() {
                if e.path().join("doc.json").exists() {
                    out.push(e.file_name().to_string_lossy().to_string());
                }
            }
        }
        out.sort();
        out
    }

    fn unique_id(&self, base: &str) -> String {
        let base = slugify(base);
        if !self.exists(&base) {
            return base;
        }
        let mut i = 2;
        loop {
            let cand = format!("{}-{}", base, i);
            if !self.exists(&cand) {
                return cand;
            }
            i += 1;
        }
    }

    fn open(&self, id: &str) -> Result<(PathBuf, Document), String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{}'", id));
        }
        let dir = self.doc_dir(id);
        if !dir.join("doc.json").exists() {
            let existing = self.doc_ids().join(", ");
            return Err(format!(
                "no document '{}'. existing: {}",
                id,
                if existing.is_empty() {
                    "(none)".into()
                } else {
                    existing
                }
            ));
        }
        let doc = Document::load(&dir)?;
        Ok((dir, doc))
    }

    // -- library ------------------------------------------------------------

    pub fn doc_create(&self, name: &str, w: u32, h: u32) -> Result<Value, String> {
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            return Err(format!(
                "canvas {w}x{h} out of range — width/height must be 1..=4096"
            ));
        }
        let id = self.unique_id(name);
        let dir = self.doc_dir(&id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut doc = Document::new(name, w, h);
        doc.save(&dir)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
        Ok(out)
    }

    pub fn doc_info(&self, id: &str) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
        Ok(out)
    }

    pub fn list_docs(&self) -> Value {
        let mut items = Vec::new();
        for id in self.doc_ids() {
            // Read doc.json directly (don't load cel images just to list).
            let meta = fs::read_to_string(self.doc_dir(&id).join("doc.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let (name, w, h, frames, layers) = match &meta {
                Some(m) => (
                    m["name"].clone(),
                    m["w"].clone(),
                    m["h"].clone(),
                    m["frames"].as_array().map(|a| a.len()).unwrap_or(0),
                    m["layers"].as_array().map(|a| a.len()).unwrap_or(0),
                ),
                None => (json!(id), json!(null), json!(null), 0, 0),
            };
            items.push(
                json!({"id": id, "name": name, "w": w, "h": h, "frames": frames, "layers": layers}),
            );
        }
        json!({"count": items.len(), "documents": items})
    }

    pub fn delete_doc(&self, id: &str) -> Result<Value, String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{}'", id));
        }
        if !self.exists(id) {
            return Err(format!("no document '{}'", id));
        }
        fs::remove_dir_all(self.doc_dir(id)).map_err(|e| e.to_string())?;
        Ok(json!({"deleted": id}))
    }

    /// Export every document as a spritesheet PNG (+ JSON meta) into a flat
    /// target dir for a game's assets/.
    pub fn export_all(&self, target_dir: &str, scale: u32) -> Result<Value, String> {
        let target = Path::new(target_dir);
        fs::create_dir_all(target).map_err(|e| e.to_string())?;
        let mut exported = Vec::new();
        for id in self.doc_ids() {
            let (_dir, doc) = self.open(&id)?;
            let out = target.join(format!("{}.png", id));
            doc.export_sheet(&out, scale.max(1))?;
            exported.push(out.to_string_lossy().to_string());
        }
        Ok(json!({"target": target.to_string_lossy(), "exported": exported}))
    }

    /// Pack every frame of every document into ONE atlas PNG (a simple shelf
    /// layout: rows of frames, wrapping at `max_width`) plus a master JSON map
    /// of `{doc_id, frame, rect:[x,y,w,h], duration_ms, pivot}` so an engine can
    /// slice the whole game's sprites from a single texture. Frames are emitted
    /// at native size × `scale`.
    pub fn export_atlas(
        &self,
        out_path: &str,
        scale: u32,
        max_width: u32,
    ) -> Result<Value, String> {
        use image::{Rgba, RgbaImage};
        let scale = scale.max(1);
        let ids = self.doc_ids();
        if ids.is_empty() {
            return Err("no documents to pack".into());
        }
        // Gather every frame image first (so we can size the atlas).
        struct Item {
            doc: String,             // owning document id
            frame: usize,            // frame index within that document
            img: RgbaImage,          // flattened (and scaled) frame pixels
            duration_ms: u32,        // frame duration, carried into the map
            pivot: Option<[i32; 2]>, // pivot in atlas-pixel space (scaled)
            boxes: Vec<Value>,       // collision boxes, pre-scaled to atlas pixels
        }
        let mut items: Vec<Item> = Vec::new();
        for id in &ids {
            let (_dir, doc) = self.open(id)?;
            for f in 0..doc.meta.frames.len() {
                let mut img = doc.flatten(f);
                if scale > 1 {
                    img = image::imageops::resize(
                        &img,
                        doc.meta.w * scale,
                        doc.meta.h * scale,
                        image::imageops::FilterType::Nearest,
                    );
                }
                items.push(Item {
                    doc: id.clone(),
                    frame: f,
                    img,
                    duration_ms: doc.meta.frames[f].duration_ms,
                    pivot: doc.meta.frames[f]
                        .pivot
                        .map(|[x, y]| [x * scale as i32, y * scale as i32]),
                    boxes: doc.meta.frames[f]
                        .boxes
                        .iter()
                        .map(|b| b.to_json(scale))
                        .collect(),
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
                "duration_ms": it.duration_ms, "pivot": it.pivot,
                "boxes": it.boxes.clone(),
            }));
        }
        if let Some(p) = Path::new(out_path).parent() {
            let _ = fs::create_dir_all(p);
        }
        atlas.save(out_path).map_err(|e| e.to_string())?;
        let meta = json!({
            "path": out_path, "atlas_w": atlas_w, "atlas_h": atlas_h,
            "count": frames_meta.len(), "frames": frames_meta,
        });
        let mp = Path::new(out_path).with_extension("json");
        std::fs::write(&mp, serde_json::to_string_pretty(&meta).unwrap())
            .map_err(|e| e.to_string())?;
        Ok(meta)
    }

    // -- structure / timeline (open -> mutate -> save) ----------------------

    fn commit(&self, dir: &Path, id: &str, mut doc: Document) -> Result<Value, String> {
        doc.save(dir)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
        Ok(out)
    }

    pub fn doc_add_layer(
        &self,
        id: &str,
        name: Option<String>,
        opacity: u8,
        blend: String,
    ) -> Result<Value, String> {
        if !crate::raster::valid_blend(&blend) {
            return Err(format!(
                "unknown blend '{blend}' — valid: {}",
                crate::raster::BLEND_NAMES
            ));
        }
        let (dir, mut doc) = self.open(id)?;
        let idx = doc.add_layer(name, opacity, blend);
        doc.save(&dir)?;
        // Slim ack — echoing the whole structure() grew O(layers×frames) per
        // call; doc_info still serves the full picture on demand.
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "added_layer": idx,
            "layers": doc.meta.layers.len(),
        }))
    }

    pub fn doc_set_layer(
        &self,
        id: &str,
        layer: usize,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<String>,
    ) -> Result<Value, String> {
        if let Some(b) = &blend {
            if !crate::raster::valid_blend(b) {
                return Err(format!(
                    "unknown blend '{b}' — valid: {}",
                    crate::raster::BLEND_NAMES
                ));
            }
        }
        let (dir, mut doc) = self.open(id)?;
        doc.set_layer(layer, visible, opacity, blend)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_add_frame(
        &self,
        id: &str,
        duration_ms: u32,
        copy_from: Option<usize>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        if let Some(src) = copy_from {
            if src >= doc.meta.frames.len() {
                return Err(format!(
                    "copy_from {src} out of range — document has {} frame(s) (0..={})",
                    doc.meta.frames.len(),
                    doc.meta.frames.len().saturating_sub(1)
                ));
            }
        }
        let idx = doc.add_frame(duration_ms, copy_from);
        doc.save(&dir)?;
        // Slim ack — echoing the whole structure() grew O(layers×frames) per
        // call during walk-cycle work; doc_info has the full picture.
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "added_frame": idx,
            "frames": doc.meta.frames.len(),
        }))
    }

    pub fn doc_set_frame_duration(&self, id: &str, frame: usize, ms: u32) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_frame_duration(frame, ms)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_add_tag(
        &self,
        id: &str,
        name: &str,
        from: usize,
        to: usize,
        direction: &str,
    ) -> Result<Value, String> {
        if !matches!(direction, "forward" | "reverse" | "pingpong") {
            return Err(format!(
                "unknown tag direction '{direction}' — valid: forward | reverse | pingpong"
            ));
        }
        let (dir, mut doc) = self.open(id)?;
        doc.add_tag(name, from, to, direction)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_fill_cel(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        color: [u8; 4],
    ) -> Result<Value, String> {
        // Masked: with an active selection this fills only the selected pixels.
        self.edit_masked(id, layer, frame, |d| d.fill_cel(layer, frame, color))
    }

    pub fn doc_clear_cel(&self, id: &str, layer: usize, frame: usize) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.clear_cel(layer, frame);
        self.commit(&dir, id, doc)
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn doc_stamp_image(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        png_path: &str,
        scale: f32,
        target_w: Option<u32>,
        rotate: f32,
        opacity: u8,
        blend: &str,
        replace: bool,
    ) -> Result<Value, String> {
        let img = image::open(png_path).map_err(|e| e.to_string())?.to_rgba8();
        // target_w wins over scale: derive the factor from the source width so
        // "stamp this at 32px wide" needs no mental math.
        let scale = match target_w {
            Some(tw) => tw.max(1) as f32 / img.width().max(1) as f32,
            None => scale,
        };
        self.edit_masked(id, layer, frame, |d| {
            d.stamp_image(
                layer, frame, x, y, img, scale, rotate, opacity, blend, replace,
            )
        })
    }

    pub fn doc_symmetry(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        vertical: Option<i32>,
        horizontal: Option<i32>,
        keep_left: bool,
        keep_top: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.symmetry(layer, frame, vertical, horizontal, keep_left, keep_top)
        })
    }

    // -- render / export ----------------------------------------------------

    /// Render a frame preview: writes the PNG to disk (for file workflows) AND
    /// returns the encoded bytes so the MCP layer can inline the image — the
    /// agent sees the pixels in the same turn instead of needing a file read.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_render(
        &self,
        id: &str,
        frame: usize,
        out_path: Option<&str>,
        scale: Option<u32>,
        region: Option<(i32, i32, i32, i32)>,
        onion: bool,
        tile: u32,
        max_size: Option<u32>,
    ) -> Result<(Vec<u8>, Value), String> {
        let (dir, doc) = self.open(id)?;
        // Adaptive default: big enough to judge a small sprite, clamped so a
        // large canvas doesn't waste vision tokens.
        let scale = scale.unwrap_or_else(|| preview_scale(doc.meta.w, doc.meta.h));
        let out = match out_path {
            Some(p) => PathBuf::from(p),
            None => dir.join(format!("preview_f{}.png", frame)),
        };
        if frame >= doc.meta.frames.len() {
            return Err(format!(
                "no frame {} (frames={})",
                frame,
                doc.meta.frames.len()
            ));
        }
        let img = doc.render_preview(frame, scale.max(1), region, onion, tile, max_size)?;
        let (w, h) = (img.width(), img.height());
        img.save(&out).map_err(|e| e.to_string())?;
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok((
            buf.into_inner(),
            json!({"path": out.to_string_lossy(), "size": [w, h], "frame": frame}),
        ))
    }

    /// Encode one cel (`layer` Some) or the flattened frame (`layer` None) as an
    /// adaptively-scaled PNG for same-turn inline previews after a mutation.
    pub fn preview_png(
        &self,
        id: &str,
        layer: Option<usize>,
        frame: usize,
    ) -> Result<Vec<u8>, String> {
        let (_dir, doc) = self.open(id)?;
        let img = doc.analysis_image(layer, frame)?;
        let sc = preview_scale(img.width(), img.height());
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
        encode_png(&scaled)
    }

    /// Flatten one frame and encode it straight to PNG bytes in memory (no file).
    /// Backs the MCP `render` resource, which serves the bytes as a blob.
    pub fn render_png_bytes(&self, id: &str, frame: usize, scale: u32) -> Result<Vec<u8>, String> {
        let (_dir, doc) = self.open(id)?;
        if frame >= doc.meta.frames.len() {
            return Err(format!(
                "no frame {} (frames={})",
                frame,
                doc.meta.frames.len()
            ));
        }
        let img = doc.render_preview(frame, scale.max(1), None, false, 1, None)?;
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(buf.into_inner())
    }

    pub fn doc_export_sheet(&self, id: &str, out_path: &str, scale: u32) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        if let Some(p) = Path::new(out_path).parent() {
            let _ = fs::create_dir_all(p);
        }
        doc.export_sheet(Path::new(out_path), scale.max(1))
    }

    pub fn doc_export_gif(
        &self,
        id: &str,
        out_path: &str,
        scale: u32,
        tag: Option<&str>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        if let Some(p) = Path::new(out_path).parent() {
            let _ = fs::create_dir_all(p);
        }
        let frames = doc.export_gif(Path::new(out_path), scale.max(1), tag)?;
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
        if let Some(p) = Path::new(out_path).parent() {
            let _ = fs::create_dir_all(p);
        }
        let frames = doc.export_apng(Path::new(out_path), scale.max(1), tag)?;
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
        let (tile_w, tile_h, scale) = (tile_w.max(1), tile_h.max(1), scale.max(1));
        let (cw, ch) = (doc.meta.w, doc.meta.h);
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
        if let Some(p) = out.parent() {
            let _ = fs::create_dir_all(p);
        }
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
        fs::write(&json_path, serde_json::to_string_pretty(&meta).unwrap())
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "path": out.to_string_lossy(), "tsx": tsx_path.to_string_lossy(),
            "json": json_path.to_string_lossy(),
            "tilecount": tilecount, "columns": columns, "rows": rows,
            "tilewidth": stw, "tileheight": sth,
        }))
    }

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

    // -- per-cel drawing ----------------------------------------------------

    fn edit<F>(&self, id: &str, f: F) -> Result<Value, String>
    where
        F: FnOnce(&mut Document) -> Result<(), String>,
    {
        let (dir, mut doc) = self.open(id)?;
        f(&mut doc)?;
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id}))
    }

    /// The active selection's mask for document `id`, validated against the
    /// canvas: Ok(None) = no selection targets this doc; Err = the selection
    /// targets this doc but no longer matches its dimensions. Erroring beats
    /// the old behaviour (silently applying the op UNMASKED), which let a
    /// paint the agent believed was confined repaint the whole cel.
    fn selection_mask_for(&self, id: &str, w: u32, h: u32) -> Result<Option<&[bool]>, String> {
        match &self.selection {
            Some(s) if s.doc_id == id => {
                if s.w != w || s.h != h {
                    Err(format!(
                        "active selection is stale: it covers a {}x{} canvas but '{}' is {}x{} — \
                         doc_select shape=none to clear it, then reselect",
                        s.w, s.h, id, w, h
                    ))
                } else {
                    Ok(Some(&s.mask))
                }
            }
            _ => Ok(None),
        }
    }

    /// Diff two canvas-sized snapshots into a mutation ack: pixel count + bbox
    /// of every change, and an explicit warning when NOTHING changed — the
    /// usual symptom of coordinates that ran off-canvas or hit the wrong cel.
    /// Blind `{ok:true}` acks let those mistakes compound between renders.
    fn change_ack(id: &str, before: &image::RgbaImage, after: &image::RgbaImage) -> Value {
        let (mut changed, mut bbox): (u64, Option<[u32; 4]>) = (0, None);
        for (b, (x, y, a)) in before.pixels().zip(after.enumerate_pixels()) {
            if b != a {
                changed += 1;
                bbox = Some(match bbox {
                    None => [x, y, x, y],
                    Some([a0, b0, c0, d0]) => [a0.min(x), b0.min(y), c0.max(x), d0.max(y)],
                });
            }
        }
        let mut out = json!({
            "ok": true,
            "doc_id": id,
            "pixels_changed": changed,
            "change_bbox": bbox.map(|b| json!(b)).unwrap_or(Value::Null),
        });
        if changed == 0 {
            out["warning"] =
                json!("no pixels changed — coordinates may be off-canvas, the colour may match what's already there, or the selection may exclude the area");
        }
        out
    }

    /// Like `edit`, but if an active selection covers this document the op `f`
    /// is confined to the selected pixels. Used by the painting ops so
    /// `doc_select` masks any of them. A stale selection (dims mismatch) is an
    /// error, never a silent unmasked apply. Returns a change ack (pixel
    /// count + bbox) instead of a blind ok.
    fn edit_masked<F>(&self, id: &str, layer: usize, frame: usize, f: F) -> Result<Value, String>
    where
        F: FnOnce(&mut Document) -> Result<(), String>,
    {
        let (dir, mut doc) = self.open(id)?;
        let before = doc.cel_full(layer, frame);
        match self.selection_mask_for(id, doc.meta.w, doc.meta.h)? {
            Some(mask) => doc.apply_masked(layer, frame, mask, f)?,
            None => f(&mut doc)?,
        }
        let after = doc.cel_full(layer, frame);
        doc.save(&dir)?;
        Ok(Self::change_ack(id, &before, &after))
    }

    pub fn doc_pencil(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        size: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.pencil(layer, frame, &points, color, size)
        })
    }

    pub fn doc_line(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: [u8; 4],
        size: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.line(layer, frame, x0, y0, x1, y1, color, size)
        })
    }

    pub fn doc_rect(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: [u8; 4],
        fill: bool,
        size: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.rect(layer, frame, x0, y0, x1, y1, color, fill, size)
        })
    }

    pub fn doc_ellipse(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        cx: i32,
        cy: i32,
        rx: i32,
        ry: i32,
        color: [u8; 4],
        fill: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.ellipse(layer, frame, cx, cy, rx, ry, color, fill)
        })
    }

    pub fn doc_polygon(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        fill: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.polygon(layer, frame, &points, color, fill)
        })
    }

    pub fn doc_polyline(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        size: i32,
        closed: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.polyline(layer, frame, &points, color, size, closed)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_stroke(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32, i32)>,
        color: [u8; 4],
        aa: bool,
        snap: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.stroke(layer, frame, &points, color, aa, snap)
        })
    }

    pub fn doc_fill(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        color: [u8; 4],
        tol: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.bucket_fill(layer, frame, x, y, color, tol)
        })
    }

    pub fn doc_replace_color(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        from: [u8; 4],
        to: [u8; 4],
        tol: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.replace_color(layer, frame, from, to, tol)
        })
    }

    pub fn doc_flip(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        horizontal: bool,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.flip(layer, frame, horizontal))
    }

    pub fn doc_shift(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        dx: i32,
        dy: i32,
        wrap: bool,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.shift(layer, frame, dx, dy, wrap))
    }

    pub fn doc_blur(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        radius: i32,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| d.blur(layer, frame, radius, region))
    }

    pub fn doc_quantize(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        palette: Vec<[u8; 4]>,
        max_colors: usize,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let pal = doc.quantize(layer, frame, palette, max_colors)?;
        doc.save(&dir)?;
        let hex: Vec<String> = pal
            .iter()
            .map(|c| format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]))
            .collect();
        Ok(json!({"doc_id": id, "count": pal.len(), "palette": pal, "hex": hex}))
    }

    pub fn doc_tween(
        &self,
        id: &str,
        from: usize,
        to: usize,
        steps: usize,
        duration_ms: u32,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let added = doc.tween(from, to, steps, duration_ms)?;
        let mut out = self.commit(&dir, id, doc)?;
        out["inserted_frames"] = json!(added);
        Ok(out)
    }

    /// Timeline lifecycle (delete | insert | duplicate | move) with cel
    /// reindexing and tag remapping — the recovery path for a bad tween.
    pub fn doc_frame_ops(
        &self,
        id: &str,
        action: &str,
        frame: usize,
        to_index: Option<usize>,
        duration_ms: Option<u32>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let out = doc.frame_ops(action, frame, to_index, duration_ms)?;
        doc.save(&dir)?;
        Ok(out)
    }

    /// Best-effort auto-checkpoint before a destructive op, labelled
    /// `auto:<tool>`, keeping only the newest few auto snapshots so repeated
    /// ops don't grow the doc dir without bound. Never fails the caller.
    pub fn auto_checkpoint(&self, id: &str, tool: &str) {
        const KEEP: usize = 5;
        let label = format!("auto:{}", tool);
        let _ = self.checkpoint(id, "save", Some(&label), None);
        if let Ok(list) = self.checkpoint(id, "list", None, None) {
            let mut autos: Vec<String> = list["checkpoints"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|c| c["label"].as_str().is_some_and(|l| l.starts_with("auto:")))
                        .filter_map(|c| c["id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            // Numeric order — lexical sort would call cp10 older than cp2.
            autos.sort_by_key(|s| {
                s.strip_prefix("cp")
                    .and_then(|t| t.parse::<u32>().ok())
                    .unwrap_or(0)
            });
            if autos.len() > KEEP {
                for cpid in &autos[..autos.len() - KEEP] {
                    let _ = self.checkpoint(id, "prune", None, Some(cpid));
                }
            }
        }
    }

    pub fn doc_outline(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        color: [u8; 4],
        aa: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| d.outline_cel(layer, frame, color, aa))
    }

    pub fn doc_drop_shadow(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        dx: i32,
        dy: i32,
        color: [u8; 4],
        opacity: u8,
        blur: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.drop_shadow(layer, frame, dx, dy, color, opacity, blur)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_glow(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        color: Option<[u8; 4]>,
        radius: i32,
        intensity: u8,
        mode: &str,
        snap: Option<crate::document::AlphaSnap>,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.glow(layer, frame, color, radius, intensity, mode)?;
            // Re-snap the continuous-tone bloom back onto the locked palette so
            // it stays crisp pixel art (the FX-palette-blowup fix).
            if let Some(a) = snap {
                if !d.meta.palette.is_empty() {
                    let pal = d.meta.palette.clone();
                    d.snap_to_palette(&pal, Some(layer), Some(frame), a);
                }
            }
            Ok(())
        })
    }

    pub fn doc_bevel(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        light: [u8; 4],
        dark: [u8; 4],
        depth: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.bevel(layer, frame, light, dark, depth)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_rim_light(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        color: [u8; 4],
        az_deg: f32,
        width: i32,
        falloff: f32,
        dark: bool,
        snap: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.rim_light(layer, frame, color, az_deg, width, falloff, dark, snap)
                .map(|_| ())
        })
    }

    pub fn doc_adjust(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        hue: f32,
        sat: f32,
        lum: f32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.adjust(layer, frame, region, hue, sat, lum)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_noise(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        kind: &str,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        scale: f32,
        octaves: u32,
        seed: u64,
        stops: Vec<(f32, [u8; 4])>,
        blend: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.noise(
                layer, frame, kind, x0, y0, x1, y1, scale, octaves, seed, stops, blend,
            )
        })
    }

    pub fn doc_bezier(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        size: i32,
        steps: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.bezier(layer, frame, &points, color, size, steps)
        })
    }

    /// Stamp `text` with the built-in 3×5 pixel font, top-left at (x,y), at
    /// integer pixel `size`. Masked by the active selection. Returns the rendered
    /// `width` in document pixels so callers can lay out the next element.
    pub fn doc_text(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        text: &str,
        color: [u8; 4],
        size: i32,
    ) -> Result<Value, String> {
        // text returns the rendered width, so thread it out via a cell rather
        // than the unit-returning edit_masked closure.
        let width = std::cell::Cell::new(0i32);
        self.edit_masked(id, layer, frame, |d| {
            width.set(d.text(layer, frame, x, y, text, color, size)?);
            Ok(())
        })?;
        Ok(json!({"ok": true, "doc_id": id, "width": width.get()}))
    }

    /// Generate a hue-shifted shading ramp from a base colour. If `set_doc` is
    /// given, also store it as that document's palette. Returns the colours.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_gradient(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        kind: &str,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        stops: Vec<(f32, [u8; 4])>,
        dither: &str,
        seed: u64,
        region: Option<(i32, i32, i32, i32)>,
        blend: bool,
        snap: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.gradient(
                layer, frame, kind, x0, y0, x1, y1, stops, dither, seed, region, blend,
            )?;
            // On-palette discipline: pull the interpolated gradient back to the
            // locked palette (RGB only — soft falloff alpha is preserved).
            if snap && !d.meta.palette.is_empty() {
                let pal = d.meta.palette.clone();
                d.snap_to_palette(
                    &pal,
                    Some(layer),
                    Some(frame),
                    crate::document::AlphaSnap::Preserve,
                );
            }
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_scatter(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        colors: Vec<[u8; 4]>,
        density: f32,
        seed: u64,
        size: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.scatter(layer, frame, x0, y0, x1, y1, &colors, density, seed, size)
        })
    }

    /// Edge-lit on-ramp shading: lit rims toward the light, core shadow away.
    /// `ramp` (dark→light) snaps each touched pixel and steps along it; without
    /// one we HSL-shift (warm highlights, cool shadows). Masked by the active
    /// selection, like the other painting ops.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_shade(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        light_dir: &str,
        steps: i32,
        region: Option<(i32, i32, i32, i32)>,
        mode: &str,
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.shade(layer, frame, light_dir, steps, region, mode, ramp)
        })
    }

    /// Volume/form shading — fill a shape's interior with a rounded light
    /// gradient snapped to a ramp (sphere/cylinder/auto). Masked by the active
    /// selection, like the other painting ops.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_form(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        light_dir: &str,
        form: &str,
        region: Option<(i32, i32, i32, i32)>,
        ramp: Option<Vec<[u8; 4]>>,
        strength: f32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.form(layer, frame, light_dir, form, region, ramp, strength)
        })
    }

    /// Two-colour ordered dither over a region. `region` is required unless an
    /// active selection covers this document (the selection then bounds it).
    /// Masked by the active selection, like the other painting ops.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_dither(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        color_a: [u8; 4],
        color_b: [u8; 4],
        pattern: &str,
        density: f32,
        only_existing: bool,
    ) -> Result<Value, String> {
        // The region defaults to the selection's bounding box when omitted; if
        // there's neither a region nor a selection it's an error (no target).
        let region = match region {
            Some(r) => r,
            None => self
                .selection_bbox(id)
                .ok_or("dither needs a `region` [x0,y0,x1,y1] unless a selection is active")?,
        };
        self.edit_masked(id, layer, frame, |d| {
            d.dither(
                layer,
                frame,
                region,
                color_a,
                color_b,
                pattern,
                density,
                only_existing,
            )
        })
    }

    /// Remove L-corner doubles from 1px strokes (Aseprite pixel-perfect cleanup).
    /// `color` (optional) restricts to strokes of that exact colour. Masked by
    /// the active selection. Returns the erased-pixel `removed` count.
    pub fn doc_pixel_perfect(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        color: Option<[u8; 4]>,
    ) -> Result<Value, String> {
        // pixel_perfect returns a count, so we thread it out via a cell rather
        // than the unit-returning edit_masked closure.
        let removed = std::cell::Cell::new(0u32);
        self.edit_masked(id, layer, frame, |d| {
            removed.set(d.pixel_perfect(layer, frame, region, color)?);
            Ok(())
        })?;
        Ok(json!({"ok": true, "doc_id": id, "removed": removed.get()}))
    }

    /// The bounding box [x0,y0,x1,y1] of the active selection on document `id`,
    /// or None when there is no matching selection (or it's empty). Lets the
    /// dither op fall back to the selected area when no explicit region.
    fn selection_bbox(&self, id: &str) -> Option<(i32, i32, i32, i32)> {
        let s = self.selection.as_ref().filter(|s| s.doc_id == id)?;
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (i, on) in s.mask.iter().enumerate() {
            if *on {
                let (x, y) = ((i as u32 % s.w) as i32, (i as u32 / s.w) as i32);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        (x0 <= x1).then_some((x0, y0, x1, y1))
    }

    /// Set/modify the active selection mask. `shape`: rect / ellipse / color /
    /// all / none. `mode` combines with the current selection: replace / add /
    /// subtract / intersect. Painting ops then confine to the `true` pixels
    /// until the selection is replaced or cleared (shape "none").
    pub fn doc_select(
        &mut self,
        id: &str,
        shape: &str,
        mode: &str,
        rect: Option<(i32, i32, i32, i32)>,
        ell: Option<(i32, i32, i32, i32)>,
        color_at: Option<ColorSelect>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let (w, h) = (doc.meta.w, doc.meta.h);
        let n = (w * h) as usize;
        if shape == "none" {
            self.selection = None;
            return Ok(json!({"doc_id": id, "selected_pixels": 0, "cleared": true}));
        }
        let idx = |x: i32, y: i32| (y as u32 * w + x as u32) as usize;
        let mut shape_mask = vec![false; n];
        match shape {
            "all" => shape_mask.iter_mut().for_each(|b| *b = true),
            "rect" => {
                let (x0, y0, x1, y1) = rect.ok_or("rect selection needs x0,y0,x1,y1")?;
                let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w as i32 - 1));
                let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h as i32 - 1));
                for y in ay..=by {
                    for x in ax..=bx {
                        shape_mask[idx(x, y)] = true;
                    }
                }
            }
            "ellipse" => {
                let (cx, cy, rx, ry) = ell.ok_or("ellipse selection needs cx,cy,rx,ry")?;
                let (a, b) = (rx.max(1) as f32 + 0.5, ry.max(1) as f32 + 0.5);
                for y in 0..h as i32 {
                    for x in 0..w as i32 {
                        if ((x - cx) as f32 / a).powi(2) + ((y - cy) as f32 / b).powi(2) <= 1.0 {
                            shape_mask[idx(x, y)] = true;
                        }
                    }
                }
            }
            "color" => {
                let c = color_at
                    .ok_or("color selection needs layer/frame and a color or sample point")?;
                let target = match (c.color, c.sample) {
                    (Some(col), _) => col,
                    (None, Some((px, py))) => doc.get_pixel(c.layer, c.frame, px, py)?,
                    (None, None) => {
                        return Err("color selection needs `color` or `x,y` to sample".into())
                    }
                };
                let near = |a: [u8; 4], b: [u8; 4]| -> bool {
                    (0..4)
                        .map(|i| (a[i] as i32 - b[i] as i32).abs())
                        .sum::<i32>()
                        <= c.tol
                };
                for y in 0..h as i32 {
                    for x in 0..w as i32 {
                        if near(doc.get_pixel(c.layer, c.frame, x, y)?, target) {
                            shape_mask[idx(x, y)] = true;
                        }
                    }
                }
            }
            other => return Err(format!("unknown selection shape '{}'", other)),
        }
        let base = match &self.selection {
            Some(s) if s.doc_id == id && s.w == w && s.h == h => s.mask.clone(),
            _ => vec![false; n],
        };
        let mask: Vec<bool> = (0..n)
            .map(|i| match mode {
                "add" => base[i] || shape_mask[i],
                "subtract" => base[i] && !shape_mask[i],
                "intersect" => base[i] && shape_mask[i],
                _ => shape_mask[i], // "replace"
            })
            .collect();
        let count = mask.iter().filter(|b| **b).count();
        self.selection = Some(Selection {
            doc_id: id.to_string(),
            w,
            h,
            mask,
        });
        Ok(json!({"doc_id": id, "selected_pixels": count, "w": w, "h": h}))
    }

    // -- selection / region / clipboard -------------------------------------

    /// Read one pixel — RGBA + a `#rrggbbaa` hex string for convenience.
    pub fn doc_get_pixel(
        &self,
        id: &str,
        layer: Option<usize>,
        frame: usize,
        x: i32,
        y: i32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        // Omitting `layer` reads the flattened composite — the visible pixel —
        // instead of one layer's cel (which reads transparent if the colour is
        // on another layer).
        let p = match layer {
            Some(l) => doc.get_pixel(l, frame, x, y)?,
            None => {
                let img = doc.flatten(frame);
                if x < 0 || y < 0 || x as u32 >= img.width() || y as u32 >= img.height() {
                    [0, 0, 0, 0]
                } else {
                    img.get_pixel(x as u32, y as u32).0
                }
            }
        };
        let hex = format!("#{:02x}{:02x}{:02x}{:02x}", p[0], p[1], p[2], p[3]);
        Ok(json!({"x": x, "y": y, "rgba": p, "hex": hex, "layer": layer}))
    }

    /// Copy a region into the shared clipboard (does not modify the document).
    pub fn doc_copy_region(
        &mut self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let (w, h, buf) = doc.copy_region(layer, frame, x0, y0, x1, y1)?;
        self.clipboard = Some((w, h, buf));
        Ok(json!({"copied": {"w": w, "h": h}}))
    }

    /// Cut = copy to clipboard, then clear the source region.
    pub fn doc_cut_region(
        &mut self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let (w, h, buf) = doc.copy_region(layer, frame, x0, y0, x1, y1)?;
        doc.clear_region(layer, frame, x0, y0, x1, y1)?;
        doc.save(&dir)?;
        self.clipboard = Some((w, h, buf));
        Ok(json!({"cut": {"w": w, "h": h}, "doc_id": id}))
    }

    /// Paste the clipboard onto a cel at (x,y). `blend` true = source-over,
    /// false = overwrite (also stamps transparency). Works across documents.
    pub fn doc_paste(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        blend: bool,
    ) -> Result<Value, String> {
        let (w, h, buf) = self
            .clipboard
            .clone()
            .ok_or("clipboard is empty — copy or cut a region first")?;
        let (dir, mut doc) = self.open(id)?;
        doc.paste_region(layer, frame, x, y, w, h, &buf, blend)?;
        doc.save(&dir)?;
        Ok(json!({"pasted": {"w": w, "h": h, "at": [x, y]}, "doc_id": id}))
    }

    // -- animation & tiling feedback (read-only) + keyframe write -----------

    /// Eased multi-frame region motion across an existing frame span. The region
    /// content at `from_frame` is stamped (source-over) into every frame in
    /// (from, to] at the eased offset; `clear_source` first clears the original
    /// rect in each destination frame. Reuses the region copy/clear/paste paths.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_keyframe_move(
        &self,
        id: &str,
        layer: usize,
        region: (i32, i32, i32, i32),
        from_frame: usize,
        to_frame: usize,
        dx: i32,
        dy: i32,
        easing: &str,
        clear_source: bool,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let offsets = doc.keyframe_move(
            layer,
            region,
            from_frame,
            to_frame,
            dx,
            dy,
            easing,
            clear_source,
        )?;
        doc.save(&dir)?;
        let offs: Vec<Value> = offsets.iter().map(|o| json!(o)).collect();
        Ok(json!({
            "doc_id": id,
            "frames_touched": offsets.len(),
            "offsets": offs,
        }))
    }

    /// Cut a region/selection of a layer onto its own part layer (above it),
    /// optionally across all frames — the rig step before keyframe_transform.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_extract_to_layer(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        use_selection: bool,
        name: Option<String>,
        all_frames: bool,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let mask = if use_selection {
            match self.selection_mask_for(id, doc.meta.w, doc.meta.h)? {
                Some(m) => Some(m.to_vec()),
                None => return Err("use_selection=true but no active selection on this doc".into()),
            }
        } else {
            None
        };
        let (new_layer, moved) =
            doc.extract_to_layer(layer, frame, region, mask.as_deref(), name, all_frames)?;
        doc.save(&dir)?;
        Ok(json!({
            "doc_id": id,
            "new_layer": new_layer,
            "pixels_moved": moved,
            "layers": doc.meta.layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>(),
        }))
    }

    /// Eased pivot rotation + translation of a region across frames — the
    /// joint-swing primitive.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_keyframe_transform(
        &self,
        id: &str,
        layer: usize,
        region: (i32, i32, i32, i32),
        pivot: (f32, f32),
        from_frame: usize,
        to_frame: usize,
        rot_deg: f32,
        dx: i32,
        dy: i32,
        easing: &str,
        snap: bool,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let placed = doc.keyframe_transform(
            layer, region, pivot, from_frame, to_frame, rot_deg, dx, dy, easing, snap,
        )?;
        doc.save(&dir)?;
        Ok(json!({
            "doc_id": id,
            "frames_touched": placed.len(),
            "placements": placed,
        }))
    }

    pub fn doc_move_region(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        dx: i32,
        dy: i32,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.move_region(layer, frame, x0, y0, x1, y1, dx, dy))
    }

    pub fn doc_clear_region(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.clear_region(layer, frame, x0, y0, x1, y1))
    }

    // -- pivots / palette ---------------------------------------------------

    pub fn doc_set_pivot(
        &self,
        id: &str,
        frame: usize,
        pivot: Option<[i32; 2]>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_pivot(frame, pivot)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_set_frame_boxes(
        &self,
        id: &str,
        frame: usize,
        boxes: Vec<crate::document::BoxMeta>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_frame_boxes(frame, boxes)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_set_palette(&self, id: &str, colors: Vec<[u8; 4]>) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_palette(colors);
        let mut out = self.commit(&dir, id, doc)?;
        out["palette_set"] = json!(true);
        Ok(out)
    }

    /// Recolour paired `from`→`to` colours across the whole document (one sprite,
    /// many palettes) in a single open→save cycle, also updating the stored
    /// palette. `from`/`to` must be the same non-empty length.
    pub fn doc_palette_swap(
        &self,
        id: &str,
        from: Vec<[u8; 4]>,
        to: Vec<[u8; 4]>,
        layer: Option<usize>,
        frame: Option<usize>,
    ) -> Result<Value, String> {
        if from.is_empty() || from.len() != to.len() {
            return Err("from/to must be non-empty and the same length".into());
        }
        let pairs: Vec<([u8; 4], [u8; 4])> = from.into_iter().zip(to).collect();
        let (dir, mut doc) = self.open(id)?;
        let changed = doc.palette_swap(&pairs, layer, frame);
        doc.save(&dir)?;
        Ok(json!({"doc_id": id, "changed": changed}))
    }

    /// Apply many ordered drawing ops to one cel in a single open→save cycle.
    /// Declarative grid painting: legend (char -> colour or palette index) +
    /// row strings paint a whole region in one call. Palette-index legends are
    /// palette-true by construction. Honours an active selection.
    pub fn doc_paint_grid(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        legend: serde_json::Map<String, Value>,
        rows: Vec<String>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let palette = doc.meta.palette.clone();
        drop(doc);
        let mut map = std::collections::HashMap::new();
        for (k, v) in &legend {
            let mut chars = k.chars();
            let ch = match (chars.next(), chars.next()) {
                (Some(c), None) => c,
                _ => return Err(format!("legend key '{}' must be a single character", k)),
            };
            if ch == '.' || ch == ' ' {
                return Err("'.' and ' ' are reserved for 'leave untouched'".into());
            }
            let color = match v {
                Value::Number(n) => {
                    let i = n.as_u64().ok_or_else(|| {
                        format!(
                            "legend '{}': palette index must be a non-negative integer",
                            k
                        )
                    })? as usize;
                    *palette.get(i).ok_or_else(|| {
                        format!(
                            "legend '{}': palette index {} out of range (palette has {})",
                            k,
                            i,
                            palette.len()
                        )
                    })?
                }
                Value::Array(a) => {
                    let c: Vec<i64> = a.iter().filter_map(|x| x.as_i64()).collect();
                    if c.len() < 3 {
                        return Err(format!(
                            "legend '{}': colour must be [r,g,b] or [r,g,b,a]",
                            k
                        ));
                    }
                    [
                        c[0] as u8,
                        c[1] as u8,
                        c[2] as u8,
                        c.get(3).copied().unwrap_or(255) as u8,
                    ]
                }
                _ => {
                    return Err(format!(
                        "legend '{}': value must be [r,g,b(,a)] or a palette index",
                        k
                    ))
                }
            };
            map.insert(ch, color);
        }
        let counts = std::cell::Cell::new((0u64, 0u64));
        let mut ack = self.edit_masked(id, layer, frame, |d| {
            counts.set(d.paint_grid(layer, frame, x, y, &map, &rows)?);
            Ok(())
        })?;
        let (mut painted, clipped) = counts.get();
        // Under an active selection, edit_masked reverts cells the mask
        // excludes AFTER paint_grid counted them — recount so `painted`
        // reports what actually landed.
        if let Ok((_, doc2)) = self.open(id) {
            let (dw, dh) = (doc2.meta.w, doc2.meta.h);
            if let Some(mask) = self.selection_mask_for(id, dw, dh)? {
                let (dwi, dhi) = (dw as i32, dh as i32);
                let (mut kept, mut masked) = (0u64, 0u64);
                for (ry, row) in rows.iter().enumerate() {
                    for (rx, ch) in row.chars().enumerate() {
                        if ch == '.' || ch == ' ' {
                            continue;
                        }
                        let (tx, ty) = (x + rx as i32, y + ry as i32);
                        if tx < 0 || ty < 0 || tx >= dwi || ty >= dhi {
                            continue;
                        }
                        match mask.get((ty * dwi + tx) as usize).copied() {
                            Some(true) => kept += 1,
                            _ => masked += 1,
                        }
                    }
                }
                if masked > 0 {
                    painted = kept;
                    ack["masked"] = json!(masked);
                    ack["warning"] = json!(format!(
                        "{} grid cells fell inside the canvas but outside the active \
                         selection and were not painted",
                        masked
                    ));
                }
            }
        }
        ack["painted"] = json!(painted);
        if clipped > 0 {
            ack["clipped"] = json!(clipped);
            ack["warning"] = json!(format!(
                "{} grid cells fell outside the canvas — check x/y and row widths",
                clipped
            ));
        }
        Ok(ack)
    }

    pub fn doc_batch(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        ops: Vec<Value>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        // Strict pre-flight: reject typo'd / wrong-shape ops up front so the whole
        // batch fails cleanly instead of silently defaulting bad params.
        for (i, op) in ops.iter().enumerate() {
            crate::document::validate_batch_op(i, op)?;
        }
        let run = |doc: &mut Document| -> Result<(), String> {
            for (i, op) in ops.iter().enumerate() {
                doc.apply_op(layer, frame, op)
                    .map_err(|e| format!("op {}: {}", i, e))?;
            }
            Ok(())
        };
        let before = doc.cel_full(layer, frame);
        match self.selection_mask_for(id, doc.meta.w, doc.meta.h)? {
            Some(mask) => doc.apply_masked(layer, frame, mask, run)?,
            None => run(&mut doc)?,
        }
        let after = doc.cel_full(layer, frame);
        doc.save(&dir)?;
        let mut ack = Self::change_ack(id, &before, &after);
        ack["ops"] = json!(ops.len());
        Ok(ack)
    }
}

/// Adaptive preview scale: aim for ~384px on the longest side (big enough for a
/// vision model to judge sprite-scale detail), clamped to 1..=16.
pub fn preview_scale(w: u32, h: u32) -> u32 {
    (384 / w.max(h).max(1)).clamp(1, 16)
}

/// Encode an RGBA image to in-memory PNG bytes.
pub fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-test-{}", tag));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn create_rejects_degenerate_and_huge_canvases() {
        let s = studio("dims");
        assert!(s.doc_create("zero", 0, 16).is_err());
        assert!(s.doc_create("zero2", 16, 0).is_err());
        assert!(s.doc_create("huge", 100000, 100000).is_err());
        assert!(s.doc_create("ok", 32, 32).is_ok());
    }

    #[test]
    fn add_layer_and_set_layer_reject_unknown_blend() {
        let s = studio("blend");
        s.doc_create("c", 8, 8).unwrap();
        assert!(s.doc_add_layer("c", None, 255, "multiply".into()).is_ok());
        assert!(s.doc_add_layer("c", None, 255, "mutiply".into()).is_err()); // typo
        assert!(s
            .doc_set_layer("c", 0, None, None, Some("glow".into()))
            .is_err());
    }

    #[test]
    fn add_tag_rejects_unknown_direction() {
        let s = studio("dir");
        s.doc_create("c", 8, 8).unwrap();
        assert!(s.doc_add_tag("c", "walk", 0, 0, "pingpong").is_ok());
        assert!(s.doc_add_tag("c", "bad", 0, 0, "ping-pong").is_err());
    }

    #[test]
    fn add_frame_rejects_out_of_range_copy_from() {
        let s = studio("cf");
        s.doc_create("c", 8, 8).unwrap();
        assert!(s.doc_add_frame("c", 100, Some(0)).is_ok());
        assert!(s.doc_add_frame("c", 100, Some(9)).is_err());
    }

    #[test]
    fn burst_dissipates_instead_of_staying_opaque() {
        let s = studio("burst");
        s.doc_create("b", 24, 24).unwrap();
        s.burst("b", 0, 12, 12, 4, 10, "ring", [255, 220, 80, 255], None)
            .unwrap();
        let (_d, doc) = s.open("b").unwrap();
        let max_alpha = |f: usize| {
            (0..24)
                .flat_map(|y| (0..24).map(move |x| (x, y)))
                .map(|(x, y)| doc.get_pixel(0, f, x, y).unwrap()[3])
                .max()
                .unwrap()
        };
        let last = doc.meta.frames.len() - 1;
        assert_eq!(max_alpha(0), 255, "flash frame should be solid");
        assert!(
            max_alpha(last) < 200,
            "rim should have faded out, got {}",
            max_alpha(last)
        );
    }

    #[test]
    fn gradient_snaps_on_palette_when_locked() {
        let s = studio("grad");
        s.doc_create("g", 16, 4).unwrap();
        let pal = vec![[20, 20, 60, 255], [200, 220, 255, 255]];
        s.doc_set_palette("g", pal.clone()).unwrap();
        // smooth (none-dither) gradient between the two palette ends, snap default on
        s.doc_gradient(
            "g",
            0,
            0,
            "linear",
            0,
            0,
            15,
            0,
            vec![(0.0, pal[0]), (1.0, pal[1])],
            "none",
            0,
            None,
            false,
            true,
        )
        .unwrap();
        let (_d, doc) = s.open("g").unwrap();
        let mut colors = std::collections::HashSet::new();
        for x in 0..16 {
            let p = doc.get_pixel(0, 0, x, 0).unwrap();
            if p[3] > 0 {
                colors.insert(p);
            }
        }
        assert!(
            colors.iter().all(|c| pal.contains(c)),
            "off-palette: {colors:?}"
        );
    }

    #[test]
    fn batch_gradient_snaps_on_palette_like_standalone() {
        // Parity fix: the batch gradient op used to skip the on-palette re-snap.
        let s = studio("batchgrad");
        s.doc_create("b", 16, 4).unwrap();
        let pal = vec![[20, 20, 60, 255], [200, 220, 255, 255]];
        s.doc_set_palette("b", pal.clone()).unwrap();
        let op = json!({
            "op": "gradient", "kind": "linear", "x0": 0, "y0": 0, "x1": 15, "y1": 0,
            "stops": [{"pos": 0.0, "color": [20, 20, 60]}, {"pos": 1.0, "color": [200, 220, 255]}],
            "dither": "none", "blend": false
        });
        s.doc_batch("b", 0, 0, vec![op]).unwrap();
        let (_d, doc) = s.open("b").unwrap();
        let mut cols = std::collections::HashSet::new();
        for x in 0..16 {
            let p = doc.get_pixel(0, 0, x, 0).unwrap();
            if p[3] > 0 {
                cols.insert(p);
            }
        }
        assert!(
            cols.iter().all(|c| pal.contains(c)),
            "batch gradient left off-palette: {cols:?}"
        );
    }

    #[test]
    fn palette_mono_and_schemes() {
        let s = studio("palette");
        let mono = s
            .palette(
                [150, 90, 70, 255],
                "mono",
                5,
                None,
                None,
                20.0,
                "arc",
                false,
                None,
            )
            .unwrap();
        assert_eq!(mono["ramps"].as_array().unwrap().len(), 1);
        assert_eq!(mono["count"], 5);
        assert_eq!(mono["validation"]["monotonic_lightness"], true);
        let tri = s
            .palette(
                [150, 90, 70, 255],
                "triadic",
                4,
                None,
                None,
                20.0,
                "arc",
                false,
                None,
            )
            .unwrap();
        assert_eq!(tri["ramps"].as_array().unwrap().len(), 3);
        assert_eq!(tri["count"], 12);
        assert!(s
            .palette(
                [1, 2, 3, 255],
                "bogus",
                5,
                None,
                None,
                20.0,
                "arc",
                false,
                None
            )
            .is_err());
        // set_doc stores the flattened palette on the document.
        s.doc_create("pd", 4, 4).unwrap();
        s.palette(
            [120, 80, 60, 255],
            "mono",
            5,
            None,
            None,
            20.0,
            "arc",
            true,
            Some("pd"),
        )
        .unwrap();
        assert_eq!(s.doc_info("pd").unwrap()["palette_len"], 5);
    }

    #[test]
    fn get_pixel_none_reads_flattened_composite() {
        let s = studio("getpix");
        s.doc_create("g", 4, 4).unwrap();
        s.doc_add_layer("g", None, 255, "normal".into()).unwrap();
        s.doc_pencil("g", 1, 0, vec![(1, 1)], [10, 20, 30, 255], 1)
            .unwrap();
        // Reading layer 0 alone misses the pixel painted on layer 1.
        assert_eq!(
            s.doc_get_pixel("g", Some(0), 0, 1, 1).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
        // Omitting layer reads the visible (flattened) pixel.
        assert_eq!(
            s.doc_get_pixel("g", None, 0, 1, 1).unwrap()["rgba"],
            json!([10, 20, 30, 255])
        );
    }

    fn humanoid_joints() -> std::collections::HashMap<String, (i32, i32)> {
        [
            ("head", (24, 8)),
            ("shoulder_l", (20, 16)),
            ("shoulder_r", (28, 16)),
            ("elbow_l", (15, 22)),
            ("elbow_r", (33, 22)),
            ("hand_l", (12, 14)),
            ("hand_r", (37, 28)),
            ("hip_l", (21, 30)),
            ("hip_r", (27, 30)),
            ("knee_l", (17, 38)),
            ("knee_r", (31, 38)),
            ("foot_l", (14, 46)),
            ("foot_r", (34, 46)),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect()
    }

    #[test]
    fn figure_is_one_connected_component() {
        let s = studio("figure");
        s.doc_create("f", 48, 48).unwrap();
        let j = humanoid_joints();
        s.figure("f", 0, 0, &j, [70, 110, 200, 255], 3, 6, 4, false, false)
            .unwrap();
        // The capsules share joint endpoints, so the whole body is one blob.
        let rep = s.doc_components("f", 0, Some(0), 8, None, 1).unwrap();
        assert_eq!(
            rep["count"], 1,
            "figure should be a single connected silhouette: {rep}"
        );
    }

    #[test]
    fn figure_rejects_missing_joint() {
        let s = studio("figure-bad");
        s.doc_create("f", 48, 48).unwrap();
        let mut j = humanoid_joints();
        j.remove("hand_l");
        assert!(s
            .figure("f", 0, 0, &j, [70, 110, 200, 255], 3, 6, 4, false, false)
            .is_err());
    }

    #[test]
    fn walk_generates_tagged_frames_with_motion() {
        let s = studio("walk");
        s.doc_create("w", 48, 48).unwrap();
        s.walk(
            "w",
            0,
            &humanoid_joints(),
            8,
            12,
            5,
            1,
            6,
            [70, 110, 200, 255],
            3,
            6,
            4,
            false,
            false,
        )
        .unwrap();
        let (_d, doc) = s.open("w").unwrap();
        assert_eq!(doc.meta.frames.len(), 8);
        assert!(
            doc.meta.tags.iter().any(|t| t.name == "walk"),
            "walk tag missing"
        );
        // frame 0 vs the half-cycle frame 4 must differ (the legs have swapped).
        let diff = (0..48)
            .flat_map(|y| (0..48).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                doc.get_pixel(0, 0, x, y).unwrap() != doc.get_pixel(0, 4, x, y).unwrap()
            })
            .count();
        assert!(
            diff > 20,
            "walk should show motion across the cycle, diff={diff}"
        );
    }

    #[test]
    #[ignore]
    fn walk_demo_renders() {
        let s = studio("walk-demo");
        s.doc_create("stroll", 48, 48).unwrap();
        s.doc_set_palette("stroll", vec![[40, 60, 120, 255], [70, 110, 200, 255]])
            .unwrap();
        s.walk(
            "stroll",
            0,
            &humanoid_joints(),
            8,
            13,
            5,
            1,
            7,
            [70, 110, 200, 255],
            3,
            7,
            4,
            true,
            true,
        )
        .unwrap();
        for f in [0usize, 2, 4, 6] {
            s.doc_render(
                "stroll",
                f,
                Some(&format!("/tmp/atelier-walk-{f}.png")),
                Some(6),
                None,
                false,
                1,
                None,
            )
            .unwrap();
        }
        println!("wrote /tmp/atelier-walk-0/2/4/6.png");
    }

    #[test]
    #[ignore]
    fn figure_demo_renders() {
        let s = studio("figure-demo");
        s.doc_create("hero", 48, 48).unwrap();
        s.doc_set_palette("hero", vec![[40, 60, 120, 255], [70, 110, 200, 255]])
            .unwrap();
        s.figure(
            "hero",
            0,
            0,
            &humanoid_joints(),
            [70, 110, 200, 255],
            3,
            7,
            4,
            true,
            true,
        )
        .unwrap();
        s.doc_render(
            "hero",
            0,
            Some("/tmp/atelier-demo-figure-hero.png"),
            Some(6),
            None,
            false,
            1,
            None,
        )
        .unwrap();
        println!("wrote /tmp/atelier-demo-figure-hero.png");
    }

    // Visual: relight rounding + burst dissipation. Ignored; run with
    // cargo test --release quality_demo2 -- --ignored --nocapture
    #[test]
    #[ignore]
    fn quality_demo2_renders() {
        let s = studio("quality-demo2");
        // Relit sphere — should read as a smooth round dome, not faceted.
        s.doc_create("sphere", 48, 48).unwrap();
        s.doc_ellipse("sphere", 0, 0, 24, 24, 16, 16, [150, 90, 70, 255], true)
            .unwrap();
        s.relight(
            "sphere",
            0,
            0,
            None,
            225.0,
            45.0,
            1.1,
            [255, 240, 210],
            0.35,
            [120, 150, 200],
            0.4,
            [255, 255, 255],
            0.25,
            [80, 90, 120],
            1.0,
            None,
        )
        .unwrap();
        s.doc_render(
            "sphere",
            0,
            Some("/tmp/atelier-demo-sphere.png"),
            Some(6),
            None,
            false,
            1,
            None,
        )
        .unwrap();
        // Burst — render a mid frame; should be a faint expanding ring.
        s.doc_create("shock", 32, 32).unwrap();
        s.burst("shock", 0, 16, 16, 5, 14, "ring", [255, 220, 90, 255], None)
            .unwrap();
        s.doc_render(
            "shock",
            3,
            Some("/tmp/atelier-demo-burst.png"),
            Some(8),
            None,
            false,
            1,
            None,
        )
        .unwrap();
        // RotSprite rotation — a 2-tone bar rotated 35° should stay crisp and
        // on-palette (no blurred fringe), not shredded.
        s.doc_create("bar", 40, 40).unwrap();
        s.doc_set_palette("bar", vec![[210, 60, 50, 255], [240, 220, 120, 255]])
            .unwrap();
        s.doc_rect("bar", 0, 0, 8, 17, 31, 22, [210, 60, 50, 255], true, 1)
            .unwrap();
        s.doc_rect("bar", 0, 0, 8, 17, 31, 19, [240, 220, 120, 255], true, 1)
            .unwrap();
        s.transform_cel(
            "bar",
            0,
            0,
            None,
            35.0,
            1.0,
            1.0,
            0.0,
            0.0,
            "rotsprite",
            true,
            true,
        )
        .unwrap();
        s.doc_render(
            "bar",
            0,
            Some("/tmp/atelier-demo-rotate.png"),
            Some(7),
            None,
            false,
            1,
            None,
        )
        .unwrap();
        let (_d, doc) = s.open("bar").unwrap();
        let mut cols = std::collections::HashSet::new();
        for y in 0..40 {
            for x in 0..40 {
                let p = doc.get_pixel(0, 0, x, y).unwrap();
                if p[3] > 0 {
                    cols.insert(p);
                }
            }
        }
        println!("ROTATED bar distinct colours: {} (palette 2)", cols.len());
        println!("wrote /tmp/atelier-demo-sphere.png /tmp/atelier-demo-burst.png /tmp/atelier-demo-rotate.png");
    }

    // Visual demo of the F1 stroke core + F2 glow-snap. Ignored by default;
    // run with: cargo test --release quality_demo -- --ignored --nocapture
    #[test]
    #[ignore]
    fn quality_demo_renders() {
        use crate::document::AlphaSnap;
        let s = studio("quality-demo");

        // --- A: a smooth tapered crescent SLASH ARC (was choppy stacked beziers)
        s.doc_create("arc", 48, 48).unwrap();
        s.doc_set_palette(
            "arc",
            vec![
                [255, 255, 255, 255],
                [120, 220, 255, 255],
                [40, 130, 220, 255],
            ],
        )
        .unwrap();
        // fat in the middle, tapering to 1px points at both tips
        let crescent = vec![
            (41, 9, 0),
            (44, 18, 5),
            (41, 28, 7),
            (33, 36, 5),
            (23, 40, 0),
        ];
        s.doc_stroke("arc", 0, 0, crescent, [120, 220, 255, 255], true, true)
            .unwrap();
        let (_b, _m) = s
            .doc_render(
                "arc",
                0,
                Some("/tmp/atelier-demo-arc.png"),
                Some(8),
                None,
                false,
                1,
                None,
            )
            .unwrap();

        // --- B: a stick figure built from CAPSULE LIMBS (connected, tapered)
        s.doc_create("figure", 48, 48).unwrap();
        s.doc_set_palette("figure", vec![[30, 30, 40, 255], [90, 90, 110, 255]])
            .unwrap();
        let limbs: Vec<Vec<(i32, i32, i32)>> = vec![
            vec![(24, 12, 7)],              // head (single round dot)
            vec![(24, 16, 6), (24, 30, 6)], // torso
            vec![(24, 19, 4), (36, 13, 3)], // sword arm, tapering
            vec![(24, 20, 4), (13, 26, 3)], // off arm
            vec![(24, 30, 5), (16, 44, 3)], // left leg
            vec![(24, 30, 5), (33, 44, 3)], // right leg
        ];
        for l in limbs {
            s.doc_stroke("figure", 0, 0, l, [30, 30, 40, 255], true, true)
                .unwrap();
        }
        s.doc_render(
            "figure",
            0,
            Some("/tmp/atelier-demo-figure.png"),
            Some(8),
            None,
            false,
            1,
            None,
        )
        .unwrap();

        // --- C: a glow orb — FX bloom auto-snapped back on-palette (F2)
        s.doc_create("orb", 32, 32).unwrap();
        let pal = vec![
            [255, 255, 255, 255],
            [180, 240, 255, 255],
            [90, 200, 255, 255],
            [30, 110, 210, 255],
        ];
        s.doc_set_palette("orb", pal.clone()).unwrap();
        s.doc_ellipse("orb", 0, 0, 16, 16, 5, 5, [255, 255, 255, 255], true)
            .unwrap();
        s.doc_glow(
            "orb",
            0,
            0,
            Some([90, 200, 255, 255]),
            5,
            220,
            "screen",
            Some(AlphaSnap::Opaque(64)),
        )
        .unwrap();
        s.doc_render(
            "orb",
            0,
            Some("/tmp/atelier-demo-orb.png"),
            Some(8),
            None,
            false,
            1,
            None,
        )
        .unwrap();

        // Count distinct opaque colours in the orb cel — must stay on-palette.
        let (_d, doc) = s.open("orb").unwrap();
        let mut colors = std::collections::HashSet::new();
        let (w, h) = (doc.meta.w, doc.meta.h);
        for y in 0..h {
            for x in 0..w {
                let p = doc.get_pixel(0, 0, x as i32, y as i32).unwrap();
                if p[3] > 0 {
                    colors.insert(p);
                }
            }
        }
        println!(
            "ORB distinct opaque colours after glow+snap: {}",
            colors.len()
        );
        assert!(
            colors.len() <= pal.len(),
            "glow snap should hold the palette ({} colours), got {}",
            pal.len(),
            colors.len()
        );
        println!("wrote /tmp/atelier-demo-arc.png /tmp/atelier-demo-figure.png /tmp/atelier-demo-orb.png");
    }

    #[test]
    fn create_persists_and_lists() {
        let s = studio("create");
        s.doc_create("Hero Sprite", 16, 16).unwrap();
        let listed = s.list_docs();
        assert_eq!(listed["count"], 1);
        // slug derived from the name
        assert_eq!(listed["documents"][0]["id"], "hero-sprite");
        // reloads from disk (open path), not just in-memory
        assert_eq!(s.doc_info("hero-sprite").unwrap()["w"], 16);
    }

    #[test]
    fn non_slug_ids_are_rejected_before_touching_disk() {
        let s = studio("traversal");
        s.doc_create("victim", 4, 4).unwrap();
        // ids arrive untrusted over MCP — path-traversal shapes must not reach fs
        for bad in ["../victim", "/etc/passwd", "a/../b", "..", "UPPER", ""] {
            assert!(
                s.doc_info(bad).unwrap_err().contains("invalid document id"),
                "{bad}"
            );
            assert!(
                s.delete_doc(bad)
                    .unwrap_err()
                    .contains("invalid document id"),
                "{bad}"
            );
        }
        // the legitimate document is untouched
        assert_eq!(s.doc_info("victim").unwrap()["w"], 4);
    }

    #[test]
    fn slugify_normalizes_names() {
        assert_eq!(slugify("Hero Sprite"), "hero-sprite");
        assert_eq!(slugify("  Multi   Space  "), "multi-space");
        assert_eq!(slugify("Weird!!Chars??"), "weird-chars");
        // empty / punctuation-only falls back
        assert_eq!(slugify(""), "untitled");
        assert_eq!(slugify("---"), "untitled");
    }

    #[test]
    fn unique_id_disambiguates_collisions() {
        let s = studio("unique");
        // three docs with the same name → suffixed slugs
        s.doc_create("dup", 4, 4).unwrap();
        s.doc_create("dup", 4, 4).unwrap();
        s.doc_create("dup", 4, 4).unwrap();
        let listed = s.list_docs();
        assert_eq!(listed["count"], 3);
        let ids: Vec<String> = listed["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["dup", "dup-2", "dup-3"]);
    }

    // Read one pixel via the Document model. The studio-level `doc_get_pixel`
    // reader lands with the analysis readers (a later step); these editing tests
    // assert on raw RGBA, so reading straight off disk keeps them identical.
    fn px(s: &Studio, id: &str, layer: usize, frame: usize, x: i32, y: i32) -> [u8; 4] {
        let (_dir, doc) = s.open(id).unwrap();
        doc.get_pixel(layer, frame, x, y).unwrap()
    }

    #[test]
    fn pivot_and_palette_persist_to_disk() {
        let s = studio("meta");
        s.doc_create("p", 8, 8).unwrap();
        s.doc_set_pivot("p", 0, Some([4, 7])).unwrap();
        s.doc_set_palette("p", vec![[1, 2, 3, 255], [4, 5, 6, 255]])
            .unwrap();
        let info = s.doc_info("p").unwrap(); // reloads from disk
        assert_eq!(info["frames"][0]["pivot"], json!([4, 7]));
        assert_eq!(info["palette_len"], 2);
    }

    #[test]
    fn frame_boxes_persist_and_export_scaled() {
        use crate::document::BoxMeta;
        let s = studio("boxes");
        s.doc_create("b", 8, 8).unwrap();
        s.doc_set_frame_boxes(
            "b",
            0,
            vec![BoxMeta {
                name: "torso".into(),
                kind: "hurt".into(),
                rect: [1, 1, 3, 4],
            }],
        )
        .unwrap();
        // Round-trips through disk in doc_info (raw, unscaled).
        let info = s.doc_info("b").unwrap();
        assert_eq!(info["frames"][0]["boxes"][0]["kind"], "hurt");
        assert_eq!(info["frames"][0]["boxes"][0]["rect"], json!([1, 1, 3, 4]));
        // Emitted scaled in the sheet sidecar.
        let out = std::env::temp_dir().join("atelier-test-boxes-sheet.png");
        let meta = s.doc_export_sheet("b", out.to_str().unwrap(), 4).unwrap();
        assert_eq!(meta["frames"][0]["boxes"][0]["rect"], json!([4, 4, 12, 16]));
    }

    #[test]
    fn batch_strict_rejects_unknown_and_missing_keys() {
        let s = studio("batchstrict");
        s.doc_create("d", 8, 8).unwrap();
        // ellipse with rect-style keys → unknown-keys error naming the op
        let bad = s.doc_batch(
            "d",
            0,
            0,
            vec![json!({"op": "ellipse", "x0": 1, "y0": 1, "x1": 8, "y1": 8, "color": [1, 2, 3]})],
        );
        let msg = bad.unwrap_err();
        assert!(msg.contains("op[0]") && msg.contains("ellipse") && msg.contains("x0"));
        // missing a required key
        assert!(s
            .doc_batch("d", 0, 0, vec![json!({"op": "ellipse"})])
            .unwrap_err()
            .contains("missing required keys"));
        // a valid batch still passes
        assert!(s
            .doc_batch(
                "d",
                0,
                0,
                vec![json!({"op": "ellipse", "cx": 4, "cy": 4, "rx": 3, "ry": 3, "color": [1, 2, 3], "fill": true})],
            )
            .is_ok());
    }

    #[test]
    fn dither_mixes_two_colors_and_respects_only_existing() {
        let s = studio("dither");
        s.doc_create("d", 8, 8).unwrap();
        let a = [10, 10, 10, 255];
        let b = [200, 200, 200, 255];
        // checker over the whole cel: both colours appear, alternating.
        s.doc_dither("d", 0, 0, Some((0, 0, 7, 7)), a, b, "checker", 0.5, false)
            .unwrap();
        let p00 = px(&s, "d", 0, 0, 0, 0);
        let p10 = px(&s, "d", 0, 0, 1, 0);
        assert_ne!(p00, p10); // chequerboard flips each step
        assert!(p00 == a || p00 == b);
        // density 1.0 floods color_b; only_existing keeps untouched art intact.
        s.doc_pencil("d", 0, 0, vec![(0, 0)], [7, 7, 7, 255], 1)
            .unwrap(); // a stray colour, neither a nor b
        s.doc_dither("d", 0, 0, Some((0, 0, 7, 7)), a, b, "bayer4", 1.0, true)
            .unwrap();
        // the stray pixel is left alone (not a or b), the rest become b
        assert_eq!(px(&s, "d", 0, 0, 0, 0), [7, 7, 7, 255]);
        assert_eq!(px(&s, "d", 0, 0, 3, 3), b);
        // no region and no selection → actionable error
        assert!(s
            .doc_dither("d", 0, 0, None, a, b, "checker", 0.5, false)
            .is_err());
    }

    #[test]
    fn shade_lights_rim_toward_light_with_ramp() {
        let s = studio("shade");
        s.doc_create("d", 6, 6).unwrap();
        // a solid mid block; ramp dark→light snaps the mid colour to index 1.
        let dark = [40, 40, 40, 255];
        let mid = [120, 120, 120, 255];
        let light = [220, 220, 220, 255];
        s.doc_rect("d", 0, 0, 1, 1, 4, 4, mid, true, 1).unwrap();
        s.doc_shade(
            "d",
            0,
            0,
            "top-left",
            1,
            None,
            "both",
            Some(vec![dark, mid, light]),
        )
        .unwrap();
        // top-left rim pixel: neighbour toward the light (-1,-1) is empty → lit.
        assert_eq!(px(&s, "d", 0, 0, 1, 1), light);
        // bottom-right rim: neighbour away from light (-1,-1) is solid but the
        // one toward the light is solid too, and away-from-light (+1,+1) is empty
        // → core shadow steps to dark.
        assert_eq!(px(&s, "d", 0, 0, 4, 4), dark);
        // an interior pixel (all neighbours opaque) is untouched.
        assert_eq!(px(&s, "d", 0, 0, 2, 2), mid);
        // bad light_dir is an actionable error
        assert!(s
            .doc_shade("d", 0, 0, "nowhere", 1, None, "both", None)
            .is_err());
    }

    #[test]
    fn pixel_perfect_removes_l_corner() {
        let s = studio("pp");
        s.doc_create("d", 6, 6).unwrap();
        let c = [255, 0, 0, 255];
        // an L: (1,1),(1,2),(2,2). The elbow (1,2) has left+? — build the classic
        // staircase elbow: horizontal then down, with the corner doubled.
        // pixels: (1,1) top, (1,2) corner, (2,2) right of corner.
        s.doc_pencil("d", 0, 0, vec![(1, 1), (1, 2), (2, 2)], c, 1)
            .unwrap();
        let r = s.doc_pixel_perfect("d", 0, 0, None, None).unwrap();
        // the corner pixel (1,2) is an L-double (top (1,1) + right (2,2) set,
        // diagonal (2,1) clear) → removed.
        assert_eq!(r["removed"], json!(1));
        assert_eq!(px(&s, "d", 0, 0, 1, 2), [0, 0, 0, 0]);
        // the two endpoints survive
        assert_eq!(px(&s, "d", 0, 0, 1, 1), c);
        assert_eq!(px(&s, "d", 0, 0, 2, 2), c);
        // colour filter ignores strokes of other colours
        let r2 = s
            .doc_pixel_perfect("d", 0, 0, None, Some([0, 255, 0, 255]))
            .unwrap();
        assert_eq!(r2["removed"], json!(0));
    }

    #[test]
    fn craft_ops_run_in_batch_and_validate_keys() {
        let s = studio("craftbatch");
        s.doc_create("d", 8, 8).unwrap();
        // valid batch: fill then dither then shade then pixel_perfect
        s.doc_batch(
            "d",
            0,
            0,
            vec![
                json!({"op": "fill_cel", "color": [120, 120, 120]}),
                json!({"op": "dither", "region": [0, 0, 7, 7], "color_a": [40, 40, 40], "color_b": [200, 200, 200], "pattern": "bayer4"}),
                json!({"op": "shade", "light_dir": "left", "steps": 1}),
                json!({"op": "pixel_perfect"}),
            ],
        )
        .unwrap();
        // dither op with a rect-style typo key is rejected, naming the op + key
        let bad = s
            .doc_batch(
                "d",
                0,
                0,
                vec![json!({"op": "dither", "color_a": [0, 0, 0], "color_b": [1, 1, 1], "x0": 0})],
            )
            .unwrap_err();
        assert!(bad.contains("op[0]") && bad.contains("dither") && bad.contains("x0"));
        // dither missing a required colour is rejected
        assert!(s
            .doc_batch(
                "d",
                0,
                0,
                vec![json!({"op": "dither", "color_a": [0, 0, 0]})]
            )
            .unwrap_err()
            .contains("missing required keys"));
    }

    #[test]
    fn clipboard_round_trips_across_documents() {
        let mut s = studio("clip");
        s.doc_create("src", 8, 8).unwrap();
        s.doc_create("dst", 8, 8).unwrap();
        // paint a pixel in src, copy a 1x1 region, paste into dst
        s.doc_pencil("src", 0, 0, vec![(2, 2)], [7, 8, 9, 255], 1)
            .unwrap();
        s.doc_copy_region("src", 0, 0, 2, 2, 2, 2).unwrap();
        s.doc_paste("dst", 0, 0, 5, 5, false).unwrap();
        let px = s.doc_get_pixel("dst", Some(0), 0, 5, 5).unwrap();
        assert_eq!(px["rgba"], json!([7, 8, 9, 255]));
        assert_eq!(px["hex"], "#070809ff");
    }

    #[test]
    fn paste_without_copy_errors() {
        let s = studio("emptyclip");
        s.doc_create("d", 4, 4).unwrap();
        assert!(s.doc_paste("d", 0, 0, 0, 0, true).is_err());
    }

    #[test]
    fn selection_confines_painting_then_clears() {
        let mut s = studio("sel");
        s.doc_create("d", 8, 8).unwrap();
        // Select a 3x3 box, then flood the whole cel: only the box fills.
        s.doc_select("d", "rect", "replace", Some((1, 1, 3, 3)), None, None)
            .unwrap();
        s.doc_fill("d", 0, 0, 0, 0, [9, 9, 9, 255], 0).unwrap();
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 2, 2).unwrap()["rgba"],
            json!([9, 9, 9, 255])
        );
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 6, 6).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
        // Clearing the selection lets a fill cover everything again.
        s.doc_select("d", "none", "replace", None, None, None)
            .unwrap();
        s.doc_fill("d", 0, 0, 0, 0, [1, 2, 3, 255], 0).unwrap();
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 6, 6).unwrap()["rgba"],
            json!([1, 2, 3, 255])
        );
    }

    #[test]
    fn stale_selection_errors_instead_of_unmasked_apply() {
        let mut s = studio("stalesel");
        s.doc_create("d", 8, 8).unwrap();
        s.doc_select("d", "rect", "replace", Some((1, 1, 3, 3)), None, None)
            .unwrap();
        // Recreate the doc at different dims: the selection is now stale.
        s.delete_doc("d").unwrap();
        s.doc_create("d", 4, 4).unwrap();
        let err = s.doc_fill("d", 0, 0, 0, 0, [9, 9, 9, 255], 0).unwrap_err();
        assert!(err.contains("stale"), "got: {err}");
        // Nothing was painted — the op refused rather than running unmasked.
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 0, 0).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
        // Clearing the selection unblocks painting.
        s.doc_select("d", "none", "replace", None, None, None)
            .unwrap();
        s.doc_fill("d", 0, 0, 0, 0, [9, 9, 9, 255], 0).unwrap();
    }

    #[test]
    fn paint_grid_paints_from_legend_and_palette_indices() {
        let s = studio("paintgrid");
        s.doc_create("d", 4, 4).unwrap();
        s.doc_set_palette("d", vec![[10, 10, 10, 255], [200, 50, 50, 255]])
            .unwrap();
        let mut legend = serde_json::Map::new();
        legend.insert("k".into(), json!(0)); // palette index
        legend.insert("r".into(), json!([200, 50, 50])); // explicit colour
        let r = s
            .doc_paint_grid(
                "d",
                0,
                0,
                0,
                0,
                legend.clone(),
                vec![".k..".into(), "kr..".into()],
            )
            .unwrap();
        assert_eq!(r["painted"], json!(3));
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 1, 0).unwrap()["rgba"],
            json!([10, 10, 10, 255])
        );
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 1, 1).unwrap()["rgba"],
            json!([200, 50, 50, 255])
        );
        // '.' left the pixel untouched.
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 0, 0).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
        // Unknown character is an actionable error; out-of-range index too.
        let err = s
            .doc_paint_grid("d", 0, 0, 0, 0, legend, vec!["z".into()])
            .unwrap_err();
        assert!(err.contains("not in the legend"));
        let mut bad = serde_json::Map::new();
        bad.insert("q".into(), json!(9));
        assert!(s
            .doc_paint_grid("d", 0, 0, 0, 0, bad, vec!["q".into()])
            .unwrap_err()
            .contains("out of range"));
    }

    #[test]
    fn paint_acks_report_changed_pixels_and_warn_on_noop() {
        let s = studio("ack");
        s.doc_create("d", 8, 8).unwrap();
        let r = s
            .doc_pencil("d", 0, 0, vec![(2, 3)], [9, 9, 9, 255], 1)
            .unwrap();
        assert_eq!(r["pixels_changed"], json!(1));
        assert_eq!(r["change_bbox"], json!([2, 3, 2, 3]));
        assert!(r.get("warning").is_none());
        // Entirely off-canvas: zero changes + an explicit warning.
        let miss = s
            .doc_pencil("d", 0, 0, vec![(50, 50)], [9, 9, 9, 255], 1)
            .unwrap();
        assert_eq!(miss["pixels_changed"], json!(0));
        assert!(miss["warning"].as_str().unwrap().contains("off-canvas"));
    }

    #[test]
    fn bucket_fill_terminates_when_fill_color_within_tolerance() {
        let s = studio("filltol");
        s.doc_create("d", 8, 8).unwrap();
        s.doc_fill_cel("d", 0, 0, [100, 100, 100, 255]).unwrap();
        // Replacement within tol of the target: the old scan re-matched its own
        // paint and looped forever; the visited mask must terminate it.
        s.doc_fill("d", 0, 0, 4, 4, [101, 100, 100, 255], 16)
            .unwrap();
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 0, 0).unwrap()["rgba"],
            json!([101, 100, 100, 255])
        );
    }

    #[test]
    fn dither_confined_to_active_selection() {
        let mut s = studio("dithersel");
        s.doc_create("d", 8, 8).unwrap();
        // select a 3x3 box; dither with no region falls back to the selection.
        s.doc_select("d", "rect", "replace", Some((1, 1, 3, 3)), None, None)
            .unwrap();
        s.doc_dither(
            "d",
            0,
            0,
            None,
            [10, 10, 10, 255],
            [200, 200, 200, 255],
            "checker",
            0.5,
            false,
        )
        .unwrap();
        // inside the selection a colour was painted; outside stays transparent.
        assert_ne!(
            s.doc_get_pixel("d", Some(0), 0, 2, 2).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
        assert_eq!(
            s.doc_get_pixel("d", Some(0), 0, 6, 6).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
    }

    #[test]
    fn atlas_packs_every_frame() {
        let s = studio("atlas");
        s.doc_create("a", 8, 8).unwrap();
        s.doc_add_frame("a", 100, None).unwrap(); // a now has 2 frames
        s.doc_create("b", 8, 8).unwrap(); // b has 1
        let out = s.docs_dir.join("atlas.png");
        let meta = s.export_atlas(out.to_str().unwrap(), 1, 64).unwrap();
        assert_eq!(meta["count"], 3); // 2 + 1 frames
        assert!(out.exists());
        assert!(out.with_extension("json").exists());
    }

    #[test]
    fn export_tileset_writes_tsx_with_tilecount_and_errors_on_indivisible() {
        let s = studio("tileset");
        // 16x16 canvas, 8x8 tiles → 2 columns × 2 rows = 4 tiles.
        s.doc_create("t", 16, 16).unwrap();
        let out = s.docs_dir.join("tiles.png");
        let meta = s
            .export_tileset("t", 8, 8, 1, out.to_str().unwrap())
            .unwrap();
        assert_eq!(meta["tilecount"], 4);
        assert_eq!(meta["columns"], 2);
        assert!(out.exists());
        let tsx_path = out.with_extension("tsx");
        let json_path = out.with_extension("json");
        assert!(tsx_path.exists() && json_path.exists());
        let tsx = fs::read_to_string(&tsx_path).unwrap();
        assert!(tsx.contains("tilecount=\"4\"") && tsx.contains("columns=\"2\""));
        // A canvas not divisible by the tile size is an actionable error.
        assert!(s
            .export_tileset("t", 5, 5, 1, out.to_str().unwrap())
            .unwrap_err()
            .contains("not divisible"));
    }

    #[test]
    fn export_tileset_escapes_xml_in_image_source() {
        let s = studio("tileset-xml");
        s.doc_create("t", 16, 16).unwrap();
        // A basename with XML metacharacters must be escaped in the .tsx source.
        let out = s.docs_dir.join("a&b\".png");
        let meta = s
            .export_tileset("t", 8, 8, 1, out.to_str().unwrap())
            .unwrap();
        let tsx_path = Path::new(meta["tsx"].as_str().unwrap());
        let tsx = fs::read_to_string(tsx_path).unwrap();
        assert!(tsx.contains("a&amp;b&quot;.png"), "tsx: {tsx}");
        assert!(!tsx.contains("a&b\".png"));
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

    #[test]
    fn palette_swap_recolours_across_frames_and_updates_palette() {
        let s = studio("palswap");
        s.doc_create("d", 8, 8).unwrap();
        s.doc_add_frame("d", 100, None).unwrap(); // two frames
        let red = [200, 0, 0, 255];
        let blue = [0, 0, 200, 255];
        // paint the same source colour into both frames, plus lock it as palette
        s.doc_pencil("d", 0, 0, vec![(1, 1)], red, 1).unwrap();
        s.doc_pencil("d", 0, 1, vec![(2, 2)], red, 1).unwrap();
        s.doc_set_palette("d", vec![red]).unwrap();
        let out = s
            .doc_palette_swap("d", vec![red], vec![blue], None, None)
            .unwrap();
        assert_eq!(out["changed"], json!(2)); // one pixel per frame
        assert_eq!(px(&s, "d", 0, 0, 1, 1), blue);
        assert_eq!(px(&s, "d", 0, 1, 2, 2), blue);
        // the stored palette entry tracked the swap
        assert_eq!(s.doc_info("d").unwrap()["palette"][0], json!(blue));
    }

    #[test]
    fn palette_swap_layer_filter_limits_scope() {
        let s = studio("palswaplayer");
        s.doc_create("d", 8, 8).unwrap();
        s.doc_add_layer("d", None, 255, "normal".into()).unwrap(); // layer 1
        let red = [200, 0, 0, 255];
        let blue = [0, 0, 200, 255];
        s.doc_pencil("d", 0, 0, vec![(1, 1)], red, 1).unwrap(); // layer 0
        s.doc_pencil("d", 1, 0, vec![(2, 2)], red, 1).unwrap(); // layer 1
        let out = s
            .doc_palette_swap("d", vec![red], vec![blue], Some(0), None)
            .unwrap();
        assert_eq!(out["changed"], json!(1)); // only layer 0 touched
        assert_eq!(px(&s, "d", 0, 0, 1, 1), blue);
        assert_eq!(px(&s, "d", 1, 0, 2, 2), red); // layer 1 untouched
    }

    #[test]
    fn palette_swap_rejects_mismatched_lengths() {
        let s = studio("palswapbad");
        s.doc_create("d", 8, 8).unwrap();
        let red = [200, 0, 0, 255];
        // one `from`, two `to` → error
        assert!(s
            .doc_palette_swap("d", vec![red], vec![red, red], None, None)
            .unwrap_err()
            .contains("same length"));
        // empty lists → error
        assert!(s.doc_palette_swap("d", vec![], vec![], None, None).is_err());
    }
}
