//! The document store: a flat library of editable pixel-art documents.
//!
//! State lives under ~/.atelier (override with ATELIER_HOME). Each document
//! is a directory `documents/<id>/` with a `doc.json` (structure + cel refs) and
//! one PNG per cel under `cels/`. There is no project/grouping layer — a document
//! is the unit, addressed by its `id` (a slug derived from its name).

// Drawing/region ops are inherently coordinate-heavy (layer, frame, x0..y1,
// colour, …); the argument-count lint fights the domain here.
#![allow(clippy::too_many_arguments)]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use atelier_core::document::Document;

mod analysis;
mod craft;
mod reference;
mod set;
mod view;
pub use view::LookOptions;

/// Hard cap on the pixel count (w×h) of an external source image. ~64 MP covers
/// any real reference/photo; the point is that a tiny-on-disk "decompression
/// bomb" (a 30000×30000 PNG is a few KB) is rejected at the header probe, before
/// its pixels are ever allocated. Shared by every `open_bounded` caller.
pub(crate) const MAX_IMPORT_PIXELS: u64 = 64 * 1024 * 1024;

/// Hard canvas ceiling: width/height a document may have (also the bound the
/// import/reference paths assume when sizing buffers).
pub(crate) const MAX_CANVAS: u32 = 4096;
/// A document's journal, beside its `doc.json` and `cels/`.
pub const JOURNAL_FILE: &str = "recipe.jsonl";
/// Text grids (silhouette/dump/diff) stay readable only so long — shared area
/// cap for every grid-emitting reader.
pub(crate) const GRID_AREA_CAP: u64 = 4096;
/// Import/reference targets above this allocate unbounded images in one call.
pub(crate) const MAX_TARGET_PIXELS: usize = 1_048_576;

/// Upper bound on an export scale factor. Canvases are already capped at 4096²
/// (`doc_create`); without a scale ceiling a `scale=64` export of that targets a
/// ~256 GB buffer. 16 matches the render/preview clamp.
pub(crate) const MAX_EXPORT_SCALE: u32 = 16;

/// Export scale when the caller leaves it unset.
pub(crate) const DEFAULT_EXPORT_SCALE: u32 = 4;

/// Clamp an export scale into `1..=MAX_EXPORT_SCALE`.
fn export_scale(scale: u32) -> u32 {
    scale.clamp(1, MAX_EXPORT_SCALE)
}

/// How a new shape combines with the selection already on the document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SelectMode {
    Replace,
    Add,
    Subtract,
    Intersect,
}

impl SelectMode {
    /// Parse a caller-supplied mode.
    ///
    /// Rejects anything unknown rather than falling back to `Replace`: a typo
    /// (`"substract"`) used to silently wipe the existing selection and replace
    /// it, which reads as a successful subtract until the export is wrong.
    pub(crate) fn parse(mode: &str) -> Result<Self, String> {
        match mode {
            "replace" => Ok(Self::Replace),
            "add" => Ok(Self::Add),
            "subtract" => Ok(Self::Subtract),
            "intersect" => Ok(Self::Intersect),
            other => Err(format!(
                "unknown selection mode '{other}' — expected replace, add, subtract or intersect"
            )),
        }
    }

    /// Combine one pixel: `base` is the current selection, `shape` the new one.
    pub(crate) fn combine(self, base: bool, shape: bool) -> bool {
        match self {
            Self::Replace => shape,
            Self::Add => base || shape,
            Self::Subtract => base && !shape,
            Self::Intersect => base && shape,
        }
    }
}

/// Open an external image with a size cap. The header dimensions are read first
/// and anything over [`MAX_IMPORT_PIXELS`] is rejected *before* decoding, and
/// the decoder is then bounded to those dimensions so a lying header can't
/// allocate past them either — an OOM / decompression-bomb guard for every
/// path that ingests a caller-supplied image (import, references, stamp).
/// Reject source dimensions whose pixel count exceeds [`MAX_IMPORT_PIXELS`].
/// Extracted so the cap is unit-testable without materialising a huge file.
fn check_import_dims(w: u32, h: u32) -> Result<(), String> {
    let px = w as u64 * h as u64;
    if px > MAX_IMPORT_PIXELS {
        return Err(format!(
            "source image is {w}x{h} = {px} px, over the {MAX_IMPORT_PIXELS}-px import cap"
        ));
    }
    Ok(())
}

pub(crate) fn open_bounded(path: &Path) -> Result<image::RgbaImage, String> {
    let dims = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    let (w, h) = dims;
    check_import_dims(w, h)?;
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(w.max(1));
    limits.max_image_height = Some(h.max(1));
    reader.limits(limits);
    Ok(reader.decode().map_err(|e| e.to_string())?.to_rgba8())
}

/// Public because `atelier replay` predicts the id an authored recipe's
/// `doc_create` would have minted (pre-journal recipes carry no minted id).
pub fn slugify(name: &str) -> String {
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

/// Escape the four XML metacharacters double-quoted attribute values need
/// (apostrophes are legal there) so values stay well-formed.
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
            .unwrap_or_else(|_| {
                // No resolvable home = a deliberate, visible choice of the temp
                // dir — not a silent relative "./.atelier" wherever the process
                // happens to run (matches the binary's service::default_home).
                dirs::home_dir()
                    .map(|h| h.join(".atelier"))
                    .unwrap_or_else(|| std::env::temp_dir().join("atelier"))
            });
        let docs_dir = home.join("documents");
        let _ = fs::create_dir_all(&docs_dir);
        Studio {
            docs_dir,
            clipboard: None,
            selection: None,
        }
    }

    /// Build a studio rooted at an explicit documents directory, bypassing the
    /// process-global `ATELIER_HOME` env var. Lets an embedder (or a test) point
    /// a studio at an arbitrary location without mutating process state.
    pub fn with_docs_dir(docs_dir: PathBuf) -> Studio {
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
        if w == 0 || h == 0 || w > MAX_CANVAS || h > MAX_CANVAS {
            return Err(format!(
                "canvas {w}x{h} out of range — width/height must be 1..={MAX_CANVAS}"
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
        self.list_docs_filtered(None, None)
    }

    /// `prefix` keeps ids starting with it (family selector: `hero-` matches
    /// `hero-idle`, `hero-run`); `contains` keeps ids with the substring. Both
    /// case-sensitive on the slug; combined = AND.
    pub fn list_docs_filtered(&self, prefix: Option<&str>, contains: Option<&str>) -> Value {
        let mut items = Vec::new();
        for id in self.doc_ids() {
            if let Some(p) = prefix {
                if !id.starts_with(p) {
                    continue;
                }
            }
            if let Some(c) = contains {
                if !id.contains(c) {
                    continue;
                }
            }
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
        if let Some(p) = Path::new(out_path).parent() {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
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

    // -- the document journal ----------------------------------------------

    /// Path of a document's journal: the ordered calls that built it.
    fn journal_path(&self, id: &str) -> PathBuf {
        self.docs_dir.join(id).join(JOURNAL_FILE)
    }

    /// Append one call to `id`'s journal.
    ///
    /// The journal is what makes "every document is a replayable recipe" true
    /// rather than aspirational: it lives beside the art it produced, so a
    /// document carries its own provenance and nothing has to be turned on
    /// beforehand to get it.
    ///
    /// JSON Lines, appended: one call per line, O(1) per write, and a killed
    /// process still leaves every completed line intact. Best-effort by design —
    /// a journal that cannot be written must never fail the drawing call that
    /// was otherwise fine.
    pub fn journal_append(&self, id: &str, tool: &str, args: &Value) {
        // Defence in depth: `id` is joined onto the store path, so validate it
        // here too rather than trust every caller forever — a bad id must never
        // write recipe.jsonl outside the store (the repo has had a traversal bug
        // before). `.is_dir()` alone would follow `../` through a real dir.
        if !Self::valid_id(id) {
            return;
        }
        let dir = self.docs_dir.join(id);
        if !dir.is_dir() {
            return; // no document, nothing to journal (e.g. a failed create)
        }
        let Ok(mut line) = serde_json::to_string(&json!({"tool": tool, "args": args})) else {
            return;
        };
        line.push('\n');
        let appended = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.journal_path(id))
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        if let Err(e) = appended {
            eprintln!("atelier: could not journal {tool} for '{id}': {e}");
        }
    }

    /// Read a document's journal back as its ordered calls.
    ///
    /// Same policy as the replay-side parser (`Recipe::parse_jsonl`): a torn
    /// FINAL line is a crash mid-append and is dropped, but a malformed line
    /// with content after it is real corruption and errors — silently skipping
    /// it would report "N steps / replayable" for a journal that `atelier
    /// replay` then refuses.
    pub fn journal(&self, id: &str) -> Result<Vec<Value>, String> {
        if !self.exists(id) {
            return Err(format!("no document '{id}'"));
        }
        let path = self.journal_path(id);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let body = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let nonempty: Vec<(usize, &str)> = body
            .lines()
            .enumerate()
            .map(|(n, l)| (n, l.trim()))
            .filter(|(_, l)| !l.is_empty())
            .collect();
        let last = nonempty.len().saturating_sub(1);
        let mut out = Vec::new();
        for (idx, (n, line)) in nonempty.iter().enumerate() {
            match serde_json::from_str(line) {
                Ok(v) => out.push(v),
                Err(_) if idx == last => break, // torn final line — crash mid-append
                Err(e) => return Err(format!("journal line {}: {e}", n + 1)),
            }
        }
        Ok(out)
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
        if !atelier_core::raster::valid_blend(&blend) {
            return Err(format!(
                "unknown blend '{blend}' — valid: {}",
                atelier_core::raster::BLEND_NAMES
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
            "layers": doc.meta().layers.len(),
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
            if !atelier_core::raster::valid_blend(b) {
                return Err(format!(
                    "unknown blend '{b}' — valid: {}",
                    atelier_core::raster::BLEND_NAMES
                ));
            }
        }
        let (dir, mut doc) = self.open(id)?;
        doc.set_layer(layer, visible, opacity, blend)?;
        self.commit(&dir, id, doc)
    }

    /// One-tool dispatch over layer structure — `op`: `add` (new layer on top) |
    /// `set` (visibility/opacity/blend of layer `index`) | `move` | `insert` |
    /// `delete` | `rename` | `duplicate` | `merge_down`. Routes to the kept
    /// `doc_add_layer` / `doc_set_layer` / `layer_ops` methods.
    /// Destructive dispatch ops must say WHICH target they hit — a defaulted
    /// index 0 silently deletes/mutates the first layer/frame.
    fn required_index(op: &str, index: Option<usize>) -> Result<usize, String> {
        index.ok_or_else(|| format!("op '{op}' needs an explicit index"))
    }

    pub fn doc_layer(
        &self,
        id: &str,
        op: &str,
        index: Option<usize>,
        to_index: Option<usize>,
        name: Option<String>,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<String>,
    ) -> Result<Value, String> {
        match op {
            "add" => self.doc_add_layer(
                id,
                name,
                opacity.unwrap_or(255),
                blend.unwrap_or_else(|| "normal".into()),
            ),
            "set" => self.doc_set_layer(
                id,
                Self::required_index(op, index)?,
                visible,
                opacity,
                blend,
            ),
            _ => self.layer_ops(
                id,
                op,
                Self::required_index(op, index)?,
                to_index,
                name,
                opacity.unwrap_or(255),
                blend.unwrap_or_else(|| "normal".into()),
            ),
        }
    }

    pub fn doc_add_frame(
        &self,
        id: &str,
        duration_ms: u32,
        copy_from: Option<usize>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        if let Some(src) = copy_from {
            if src >= doc.meta().frames.len() {
                return Err(format!(
                    "copy_from {src} out of range — document has {} frame(s) (0..={})",
                    doc.meta().frames.len(),
                    doc.meta().frames.len().saturating_sub(1)
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
            "frames": doc.meta().frames.len(),
        }))
    }

    pub fn doc_set_frame_duration(&self, id: &str, frame: usize, ms: u32) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_frame_duration(frame, ms)?;
        self.commit(&dir, id, doc)
    }

    /// One-tool dispatch over frame lifecycle + timing — `op`: `add` (append,
    /// optional `copy_from`) | `duration` (set frame `frame`'s ms) | `delete` |
    /// `insert` | `duplicate` | `move`. Routes to the kept `doc_add_frame` /
    /// `doc_set_frame_duration` / `doc_frame_ops`. (Pivot, boxes, tags and
    /// keyframe motion keep their own tools.)
    pub fn doc_frame(
        &self,
        id: &str,
        op: &str,
        frame: Option<usize>,
        copy_from: Option<usize>,
        to_index: Option<usize>,
        duration_ms: Option<u32>,
    ) -> Result<Value, String> {
        match op {
            "add" => self.doc_add_frame(
                id,
                duration_ms.unwrap_or(atelier_core::document::DEFAULT_FRAME_MS),
                copy_from,
            ),
            "duration" => self.doc_set_frame_duration(
                id,
                Self::required_index(op, frame)?,
                duration_ms.unwrap_or(atelier_core::document::DEFAULT_FRAME_MS),
            ),
            _ => self.doc_frame_ops(
                id,
                op,
                Self::required_index(op, frame)?,
                to_index,
                duration_ms,
            ),
        }
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

    pub fn doc_clear_cel(&self, id: &str, layer: usize, frame: usize) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.clear_cel(layer, frame)?;
        self.commit(&dir, id, doc)
    }

    // -- render / export ----------------------------------------------------

    /// Flatten one frame and encode it straight to PNG bytes in memory (no file).
    /// Backs the MCP `render` resource, which serves the bytes as a blob.
    pub fn render_png_bytes(&self, id: &str, frame: usize, scale: u32) -> Result<Vec<u8>, String> {
        let (_dir, doc) = self.open(id)?;
        if frame >= doc.meta().frames.len() {
            return Err(format!(
                "no frame {} (frames={})",
                frame,
                doc.meta().frames.len()
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
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
        doc.export_sheet(Path::new(out_path), export_scale(scale))
    }

    /// One-tool dispatch over the per-document file exports — `op`: `sheet` |
    /// `anim` | `tileset`. Shared `out_path`/`scale`; op-specific params come
    /// flattened (sheet: `meta`; anim: `format`,`tag`; tileset: `tile_w`,
    /// `tile_h`). The library-wide exports are the sibling [`Self::export_all`]
    /// and [`Self::export_atlas`], which the MCP layer fuses onto the same tool
    /// as `doc_export op=all|atlas`; generators (`wang`) stay their own tool.
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
                    if let Some(p) = Path::new(out_path).parent() {
                        fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
                    }
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
                "doc_export: unknown op '{other}' — use sheet|anim|tileset (wang/atlas/all are their own tools)"
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
        if let Some(p) = Path::new(out_path).parent() {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
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
        if let Some(p) = Path::new(out_path).parent() {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
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
        if let Some(p) = out.parent() {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
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
        match self.selection_mask_for(id, doc.meta().w, doc.meta().h)? {
            Some(mask) => doc.apply_masked(layer, frame, mask, f)?,
            None => f(&mut doc)?,
        }
        let after = doc.cel_full(layer, frame);
        doc.save(&dir)?;
        Ok(Self::change_ack(id, &before, &after))
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
        let mode = SelectMode::parse(mode)?;
        let (_dir, doc) = self.open(id)?;
        let (w, h) = (doc.meta().w, doc.meta().h);
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
                // One cel read for the whole scan — get_pixel would re-probe
                // the cel map per pixel.
                let img = doc.analysis_image(Some(c.layer), c.frame)?;
                for y in 0..h as i32 {
                    for x in 0..w as i32 {
                        if near(img.get_pixel(x as u32, y as u32).0, target) {
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
            .map(|i| mode.combine(base[i], shape_mask[i]))
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

    /// Read one pixel — RGBA + a `#rrggbbaa` hex string. Test-only helper; the
    /// `doc_get_pixel` tool was removed (use `doc_dump_region` in production).
    #[cfg(test)]
    pub fn doc_get_pixel(
        &self,
        id: &str,
        layer: Option<usize>,
        frame: usize,
        x: i32,
        y: i32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
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
        let hex = crate::hex_rgba(&p);
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
            .as_ref()
            .ok_or("clipboard is empty — copy or cut a region first")?;
        self.doc_paste_pixels(id, layer, frame, x, y, *w, *h, buf, blend)
    }

    /// Paste explicit pixels without touching the shared clipboard. This is
    /// the replay path: a journaled paste step embeds the pixels it used, so a
    /// document's journal stays self-contained even when the copy came from
    /// another document (which the per-document journal cannot express).
    #[allow(clippy::too_many_arguments)]
    pub fn doc_paste_pixels(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        buf: &[u8],
        blend: bool,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.paste_region(layer, frame, x, y, w, h, buf, blend)?;
        doc.save(&dir)?;
        Ok(json!({"pasted": {"w": w, "h": h, "at": [x, y]}, "doc_id": id}))
    }

    /// The current clipboard, if any — the journal embeds it into each paste
    /// step it records (see `doc_paste_pixels`).
    pub fn clipboard_pixels(&self) -> Option<(u32, u32, &[u8])> {
        self.clipboard.as_ref().map(|(w, h, b)| (*w, *h, &b[..]))
    }

    // -- animation & tiling feedback (read-only) + keyframe write -----------

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

    /// One-tool dispatch over region + clipboard ops — `op`: `copy` | `cut` |
    /// `clear` (the `[x0,y0,x1,y1]` rect) | `move` (rect by `dx,dy`) | `paste`
    /// (the clipboard at `x,y`, `blend` source-over by default). Routes to the
    /// kept region methods.
    pub fn doc_region(
        &mut self,
        id: &str,
        op: &str,
        layer: usize,
        frame: usize,
        x0: Option<i32>,
        y0: Option<i32>,
        x1: Option<i32>,
        y1: Option<i32>,
        dx: Option<i32>,
        dy: Option<i32>,
        x: Option<i32>,
        y: Option<i32>,
        blend: Option<bool>,
    ) -> Result<Value, String> {
        // The rect ops act on whatever region they are given — a defaulted 0
        // would silently target the top-left corner, so the corners are required.
        let rect = |name: &str| -> Result<(i32, i32, i32, i32), String> {
            match (x0, y0, x1, y1) {
                (Some(x0), Some(y0), Some(x1), Some(y1)) => Ok((x0, y0, x1, y1)),
                _ => Err(format!("doc_region op '{name}' needs x0/y0/x1/y1")),
            }
        };
        match op {
            "copy" => {
                let (x0, y0, x1, y1) = rect(op)?;
                self.doc_copy_region(id, layer, frame, x0, y0, x1, y1)
            }
            "cut" => {
                let (x0, y0, x1, y1) = rect(op)?;
                self.doc_cut_region(id, layer, frame, x0, y0, x1, y1)
            }
            "clear" => {
                let (x0, y0, x1, y1) = rect(op)?;
                self.doc_clear_region(id, layer, frame, x0, y0, x1, y1)
            }
            "move" => {
                let (x0, y0, x1, y1) = rect(op)?;
                let (dx, dy) = dx.zip(dy).ok_or("doc_region op 'move' needs dx/dy")?;
                self.doc_move_region(id, layer, frame, x0, y0, x1, y1, dx, dy)
            }
            "paste" => self.doc_paste(
                id,
                layer,
                frame,
                x.unwrap_or(0),
                y.unwrap_or(0),
                blend.unwrap_or(true),
            ),
            other => Err(format!(
                "doc_region: unknown op '{other}' — use copy|cut|paste|move|clear"
            )),
        }
    }

    // -- palette -------------------------------------------------------------

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
        let palette = doc.meta().palette.clone();
        let (dw, dh) = (doc.meta().w, doc.meta().h);
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
        // reports what actually landed (dims captured above; no third load).
        {
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
            atelier_core::document::validate_batch_op(i, op)?;
        }
        let run = |doc: &mut Document| -> Result<(), String> {
            for (i, op) in ops.iter().enumerate() {
                doc.apply_op(layer, frame, op)
                    .map_err(|e| format!("op {}: {}", i, e))?;
            }
            Ok(())
        };
        let before = doc.cel_full(layer, frame);
        match self.selection_mask_for(id, doc.meta().w, doc.meta().h)? {
            Some(mask) => doc.apply_masked(layer, frame, mask, run)?,
            None => run(&mut doc)?,
        }
        let after = doc.cel_full(layer, frame);
        doc.save(&dir)?;
        let mut ack = Self::change_ack(id, &before, &after);
        ack["ops"] = json!(ops.len());
        Ok(ack)
    }

    /// Apply ONE drawing op to a cel — the single-op form of [`Self::doc_batch`],
    /// scoped to the "add marks" vocabulary (geometry, fills, text, procedural).
    /// `params` is the op's flattened args; the op name is injected and the call
    /// routes through the same validate-and-apply path a one-element batch uses,
    /// so there is one source of truth for the op schema and behaviour.
    pub fn doc_draw(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        op: &str,
        mut params: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        use atelier_core::document::DRAW_OPS;
        if !DRAW_OPS.contains(&op) {
            return Err(format!(
                "doc_draw: '{op}' is not a draw op — use one of [{}] (filters and lighting live on their own tools)",
                DRAW_OPS.join(", ")
            ));
        }
        params.insert("op".into(), json!(op));
        self.doc_batch(id, layer, frame, vec![Value::Object(params)])
    }

    /// Apply ONE transform/effect op to a cel — the single-op form of
    /// [`doc_batch`](Self::doc_batch) for the ops that REWORK existing pixels
    /// (filters, lighting, colour, geometry); the complement of
    /// [`doc_draw`](Self::doc_draw), which adds new marks. Same validated dispatch.
    /// (`glow` is batch-only — its on-palette `snap` is not a single-op form.)
    pub fn doc_fx(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        op: &str,
        mut params: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        use atelier_core::document::FX_OPS;
        if !FX_OPS.contains(&op) {
            return Err(format!(
                "doc_fx: '{op}' is not an fx op — use one of [{}] (drawing marks → doc_draw; glow is a batch-only op — call it inside doc_batch)",
                FX_OPS.join(", ")
            ));
        }
        params.insert("op".into(), json!(op));
        self.doc_batch(id, layer, frame, vec![Value::Object(params)])
    }
}

/// Adaptive preview scale: aim for ~384px on the longest side (big enough for a
/// vision model to judge sprite-scale detail), clamped to 1..=16.
pub(crate) fn preview_scale(w: u32, h: u32) -> u32 {
    (384 / w.max(h).max(1)).clamp(1, 16)
}

/// `#rrggbb` — the one place the report hex format lives.
pub(crate) fn hex_rgb(c: &[u8]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `#rrggbbaa` — hex with alpha, for translucency-aware reports.
pub(crate) fn hex_rgba(c: &[u8]) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
}

/// Nearest-neighbour upscale (keeps the pixel grid crisp).
///
/// The scale is clamped to `MAX_EXPORT_SCALE` here rather than at each of the
/// nine call sites: `doc_look`, `select_render`, `contact_sheet` and the diff
/// overlays all took it straight from the caller, and `scale: 1000000000`
/// overflowed the dimension multiply — a panic in debug, a multi-terabyte
/// allocation in release, and either way a poisoned studio lock that broke every
/// later call. One choke point cannot be forgotten.
pub(crate) fn scale_nn(img: &image::RgbaImage, scale: u32) -> image::RgbaImage {
    let scale = export_scale(scale);
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

/// Encode an RGBA image to in-memory PNG bytes.
pub(crate) fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
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
    fn a_document_journals_the_calls_that_built_it() {
        let s = studio("journal");
        s.doc_create("d", 8, 8).unwrap();
        assert!(s.journal("d").unwrap().is_empty(), "nothing recorded yet");

        s.journal_append("d", "doc_create", &json!({"name": "d"}));
        s.journal_append("d", "doc_draw", &json!({"op": "rect"}));
        let steps = s.journal("d").unwrap();
        assert_eq!(steps.len(), 2, "appends accumulate in order");
        assert_eq!(steps[0]["tool"], "doc_create");
        assert_eq!(steps[1]["args"]["op"], "rect");

        // Journaling an unknown document is a no-op, never a panic or a stray
        // directory: a failed create must not leave a journal behind.
        s.journal_append("nope", "doc_draw", &json!({}));
        assert!(s.journal("nope").is_err(), "no document, no journal");
    }

    #[test]
    fn journal_read_policy_matches_the_replay_parser() {
        // Torn FINAL line = crash mid-append, tolerated. Mid-file corruption =
        // error — silently skipping it would list steps that replay refuses.
        let s = studio("journal-policy");
        s.doc_create("d", 8, 8).unwrap();
        s.journal_append("d", "doc_create", &json!({"name": "d"}));
        s.journal_append("d", "doc_draw", &json!({"op": "rect"}));
        let path = s.journal_path("d");

        let clean = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{clean}{{\"tool\":\"doc_")).unwrap();
        assert_eq!(s.journal("d").unwrap().len(), 2, "torn final line dropped");

        fs::write(&path, format!("not json\n{clean}")).unwrap();
        let err = s.journal("d").unwrap_err();
        assert!(err.contains("line 1"), "mid-file corruption errors: {err}");
    }
}
