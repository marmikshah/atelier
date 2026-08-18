//! The editable document model — atelier's layered, animated core.
//!
//! A `Document` is a canvas of ordered **layers** (opacity / visibility / blend)
//! over a timeline of **frames** (each with a duration). A **cel** is one
//! layer×frame image placed at (x,y); cels are sparse. The document also holds a
//! **palette** and animation **tags** (named frame ranges).
//!
//! Persistence: a directory with `doc.json` (structure + cel file refs) and one
//! PNG per cel under `cels/`. Rendering flattens visible layers at a frame with
//! source-over compositing scaled by layer opacity; export covers spritesheets
//! (+ JSON sidecars) and animated GIF/APNG.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use image::{Rgba, RgbaImage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::raster;

mod draw;
mod export;
mod fx;
mod operation;
mod palette;
mod region;
mod render;
mod timeline;

#[cfg(test)]
mod tests;

pub use fx::{DitherAxis, DitherPattern};
pub use operation::{OpSide, color_array, draw_ops, fx_ops, operation_schema, validate_op};
pub use render::{ValueView, seam_axis_img};
pub use timeline::FrameAction;

/// Current persisted `doc.json` format.
pub const DOCUMENT_FORMAT_VERSION: u32 = 1;
/// Maximum width or height accepted by both constructors and persisted files.
pub const MAX_DOCUMENT_DIMENSION: u32 = 4096;
/// Largest accepted persisted `doc.json` file.
pub const MAX_DOCUMENT_METADATA_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum fractal layers accepted by the procedural noise operation.
///
/// Work is linear in this value for every painted pixel; layers beyond this
/// point are sub-pixel detail on the largest supported canvas.
pub const MAX_NOISE_OCTAVES: u32 = 16;
/// Maximum colour count accepted by palette-driven operations and documents.
pub const MAX_PALETTE_COLORS: usize = 256;
/// Maximum generated palette size for the quantize operation.
pub const MAX_QUANTIZE_COLORS: usize = MAX_PALETTE_COLORS;
/// Maximum colour stops accepted by gradient-driven operations.
pub const MAX_GRADIENT_STOPS: usize = 64;
/// Maximum UTF-8 byte length of document, layer, and animation-tag names.
pub const MAX_DOCUMENT_NAME_BYTES: usize = 1024;
/// Maximum number of layers stored in one document.
pub const MAX_DOCUMENT_LAYERS: usize = 256;
/// Maximum number of animation frames stored in one document.
pub const MAX_DOCUMENT_FRAMES: usize = 4096;
/// Maximum number of animation tags stored in one document.
pub const MAX_DOCUMENT_TAGS: usize = 4096;
/// Maximum number of materialized layer/frame cels stored in one document.
pub const MAX_DOCUMENT_CELS: usize = 16_384;
/// Maximum aggregate decoded cel pixels held by one loaded document.
///
/// RGBA8 uses four bytes per pixel, so this bounds the cel map itself to
/// 256 MiB before hash-map and metadata overhead.
pub const MAX_DOCUMENT_CEL_PIXELS: u64 = 64 * 1024 * 1024;

const fn document_format_v1() -> u32 {
    DOCUMENT_FORMAT_VERSION
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LayerMeta {
    pub name: String,
    pub opacity: u8,
    pub visible: bool,
    /// Compositing mode: normal/multiply/screen/add/overlay/soft-light/
    /// hard-light/darken/lighten/color-dodge/color-burn/difference/subtract/
    /// exclusion. Only canonical values are accepted.
    pub blend: raster::Blend,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct FrameMeta {
    pub duration_ms: u32,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TagDirection {
    #[default]
    Forward,
    Reverse,
    Pingpong,
}

impl TagDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Reverse => "reverse",
            Self::Pingpong => "pingpong",
        }
    }
}

impl std::fmt::Display for TagDirection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TagMeta {
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub direction: TagDirection,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CelMeta {
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub file: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DocMeta {
    /// Explicit on-disk contract version. Legacy pre-version files deserialize
    /// as v1; newer unknown versions fail instead of being reinterpreted.
    #[serde(default = "document_format_v1")]
    pub format_version: u32,
    pub name: String,
    pub w: u32,
    pub h: u32,
    pub palette: Vec<[u8; 4]>,
    pub layers: Vec<LayerMeta>,
    pub frames: Vec<FrameMeta>,
    pub tags: Vec<TagMeta>,
    pub cels: Vec<CelMeta>,
    /// Reference image filename inside the doc dir (`doc_ref op=set`).
    /// — the original the artwork is recreating, kept for compare loops.
    pub reference: Option<String>,
}

impl DocMeta {
    /// Validate the one current persisted metadata contract.
    pub fn validate(&self) -> Result<(), String> {
        validate_meta(self)
    }
}

/// A loaded document: structure + the cel images in memory.
pub struct Document {
    pub(crate) meta: DocMeta,
    /// (layer, frame) -> (x, y, image)
    cels: CelMap,
    /// Cels whose pixels changed since load (or that were re-keyed by a
    /// structural op). `save` writes only these — plus any cel whose file is
    /// missing — instead of re-encoding the whole document per tool call.
    dirty: HashSet<(usize, usize)>,
}

type CelMap = HashMap<(usize, usize), (i32, i32, RgbaImage)>;

/// A read-only document snapshot containing only the cels requested for
/// analysis.
///
/// Unlike [`Document`], this type cannot be mutated or saved. That keeps a
/// partial cel set from ever replacing the complete persisted document while
/// allowing one-frame and one-layer readers to avoid decoding unrelated PNGs.
pub struct AnalysisDocument {
    meta: DocMeta,
    cels: CelMap,
    frames: HashSet<usize>,
    layer: Option<usize>,
}

/// Result of `frame_diff_region`: `(added, removed, recolored, change_bbox,
/// image_a, image_b)` — the change tallies, the bbox of all changed pixels, and
/// both analysis images so callers can also render a grid/overlay.
pub type FrameDiff = (u32, u32, u32, Option<[i32; 4]>, RgbaImage, RgbaImage);

fn cel_file(layer: usize, frame: usize) -> String {
    format!("cels/L{}_F{}.png", layer, frame)
}

fn open_regular_file(path: &Path, label: &str) -> Result<File, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {label} '{}': {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "{label} '{}' must be a regular file (symlinks are refused)",
            path.display()
        ));
    }

    let mut options = File::options();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // Linux O_NOFOLLOW closes the final-component symlink race between
        // the metadata check above and the file open.
        options.custom_flags(0o400000);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("cannot open {label} '{}': {error}", path.display()))?;

    // Detect a replacement between the path check and open. The supported
    // native target is Linux; keep a conservative fallback for library users.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let opened = file.metadata().map_err(|error| {
            format!("cannot inspect open {label} '{}': {error}", path.display())
        })?;
        if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
            return Err(format!(
                "{label} '{}' changed while it was being opened",
                path.display()
            ));
        }
    }

    Ok(file)
}

fn read_regular_utf8_bounded(path: &Path, label: &str, max_bytes: u64) -> Result<String, String> {
    let file = open_regular_file(path, label)?;
    let size = file
        .metadata()
        .map_err(|error| format!("cannot inspect {label} '{}': {error}", path.display()))?
        .len();
    if size > max_bytes {
        return Err(format!(
            "{label} '{}' is {size} bytes; limit is {max_bytes} bytes",
            path.display()
        ));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {label} '{}': {error}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "{label} '{}' grew beyond the {max_bytes}-byte limit while it was read",
            path.display()
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("{label} '{}' is not UTF-8: {error}", path.display()))
}

fn require_real_directory(path: &Path, label: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {label} '{}': {error}", path.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "{label} '{}' must be a real directory (symlinks are refused)",
            path.display()
        ));
    }
    Ok(())
}

fn ensure_real_directory(path: &Path, label: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => require_real_directory(path, label),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)
                .map_err(|error| format!("cannot create {label} '{}': {error}", path.display()))?;
            require_real_directory(path, label)
        }
        Err(error) => Err(format!(
            "cannot inspect {label} '{}': {error}",
            path.display()
        )),
    }
}

fn check_name_size(kind: &str, name: &str) -> Result<(), String> {
    if name.len() > MAX_DOCUMENT_NAME_BYTES {
        return Err(format!(
            "{kind} is {} UTF-8 bytes; limit is {MAX_DOCUMENT_NAME_BYTES} bytes",
            name.len()
        ));
    }
    Ok(())
}

fn checked_cel_pixel_total(
    dimensions: impl IntoIterator<Item = (u64, u64)>,
) -> Result<u64, String> {
    let mut total = 0u64;
    for (width, height) in dimensions {
        let pixels = width
            .checked_mul(height)
            .ok_or("cel dimensions overflow the aggregate pixel count")?;
        total = total
            .checked_add(pixels)
            .ok_or("aggregate cel pixel count overflowed")?;
        if total > MAX_DOCUMENT_CEL_PIXELS {
            return Err(format!(
                "document cels contain {total} decoded pixels; limit is {MAX_DOCUMENT_CEL_PIXELS} pixels (256 MiB RGBA8)"
            ));
        }
    }
    Ok(total)
}

type ProbedCel = (usize, usize, i32, i32, std::path::PathBuf, (u32, u32), bool);

/// Validate every stored cel while decoding only metadata entries accepted by
/// `select`. Header probes retain the complete document's nofollow, dimension,
/// and aggregate decoded-pixel checks before any selected image is retained.
fn load_stored_cels(
    dir: &Path,
    meta: &DocMeta,
    select: impl Fn(&CelMeta) -> bool,
) -> Result<CelMap, String> {
    if let Some(reference) = &meta.reference {
        open_regular_file(&dir.join(reference), "stored reference")?;
    }
    let cels_dir = dir.join("cels");
    match std::fs::symlink_metadata(&cels_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && meta.cels.is_empty() => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect stored cels directory '{}': {error}",
                cels_dir.display()
            ));
        }
        Ok(_) => require_real_directory(&cels_dir, "stored cels directory")?,
    }

    let mut probed_cels: Vec<ProbedCel> = Vec::with_capacity(meta.cels.len());
    let mut selected_count = 0usize;
    for cel in &meta.cels {
        let selected = select(cel);
        selected_count += usize::from(selected);
        let path = dir.join(&cel.file);
        let dimensions =
            image::ImageReader::new(BufReader::new(open_regular_file(&path, "stored cel")?))
                .with_guessed_format()
                .map_err(|error| error.to_string())?
                .into_dimensions()
                .map_err(|error| error.to_string())?;
        raster::checked_rgba_dimensions(
            "stored cel",
            u64::from(dimensions.0),
            u64::from(dimensions.1),
        )?;
        probed_cels.push((
            cel.layer, cel.frame, cel.x, cel.y, path, dimensions, selected,
        ));
    }
    checked_cel_pixel_total(
        probed_cels
            .iter()
            .map(|(_, _, _, _, _, (width, height), _)| (u64::from(*width), u64::from(*height))),
    )?;

    let mut cels = HashMap::with_capacity(selected_count);
    for (layer, frame, x, y, path, dimensions, selected) in probed_cels {
        if !selected {
            continue;
        }
        let mut reader =
            image::ImageReader::new(BufReader::new(open_regular_file(&path, "stored cel")?))
                .with_guessed_format()
                .map_err(|error| error.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(dimensions.0);
        limits.max_image_height = Some(dimensions.1);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|error| error.to_string())?
            .to_rgba8();
        if image.dimensions() != dimensions {
            return Err(format!(
                "stored cel '{}' changed dimensions while it was loaded",
                path.display()
            ));
        }
        cels.insert((layer, frame), (x, y, image));
    }
    Ok(cels)
}

/// Validate the one current on-disk document shape. Loading does not repair,
/// default, or reinterpret persisted metadata.
fn validate_meta(meta: &DocMeta) -> Result<(), String> {
    if meta.format_version != DOCUMENT_FORMAT_VERSION {
        return Err(format!(
            "unsupported document format {} (this build supports {})",
            meta.format_version, DOCUMENT_FORMAT_VERSION
        ));
    }
    if meta.w == 0
        || meta.h == 0
        || meta.w > MAX_DOCUMENT_DIMENSION
        || meta.h > MAX_DOCUMENT_DIMENSION
    {
        return Err(format!(
            "document dimensions must be 1..={MAX_DOCUMENT_DIMENSION}, got {}x{}",
            meta.w, meta.h,
        ));
    }
    check_name_size("document name", &meta.name)?;
    if meta.palette.len() > MAX_PALETTE_COLORS {
        return Err(format!(
            "document palette has {} colours; limit is {MAX_PALETTE_COLORS}",
            meta.palette.len()
        ));
    }
    if meta.layers.is_empty() {
        return Err("document must contain at least one layer".into());
    }
    if meta.layers.len() > MAX_DOCUMENT_LAYERS {
        return Err(format!(
            "document has {} layers; limit is {MAX_DOCUMENT_LAYERS}",
            meta.layers.len()
        ));
    }
    for layer in &meta.layers {
        check_name_size("layer name", &layer.name)?;
    }
    if meta.frames.is_empty() {
        return Err("document must contain at least one frame".into());
    }
    if meta.frames.len() > MAX_DOCUMENT_FRAMES {
        return Err(format!(
            "document has {} frames; limit is {MAX_DOCUMENT_FRAMES}",
            meta.frames.len()
        ));
    }
    if meta.tags.len() > MAX_DOCUMENT_TAGS {
        return Err(format!(
            "document has {} tags; limit is {MAX_DOCUMENT_TAGS}",
            meta.tags.len()
        ));
    }
    for tag in &meta.tags {
        check_name_size("tag name", &tag.name)?;
        if tag.from > tag.to || tag.to >= meta.frames.len() {
            return Err(format!(
                "tag '{}' range {}..{} is outside {} frame(s)",
                tag.name,
                tag.from,
                tag.to,
                meta.frames.len()
            ));
        }
    }
    if meta.cels.len() > MAX_DOCUMENT_CELS {
        return Err(format!(
            "document has {} cels; limit is {MAX_DOCUMENT_CELS}",
            meta.cels.len()
        ));
    }
    let mut cel_keys = HashSet::new();
    for cel in &meta.cels {
        if cel.layer >= meta.layers.len() || cel.frame >= meta.frames.len() {
            return Err(format!(
                "cel ({},{}) is outside {} layer(s) and {} frame(s)",
                cel.layer,
                cel.frame,
                meta.layers.len(),
                meta.frames.len()
            ));
        }
        if !cel_keys.insert((cel.layer, cel.frame)) {
            return Err(format!(
                "duplicate cel metadata for ({},{})",
                cel.layer, cel.frame
            ));
        }
        let expected = cel_file(cel.layer, cel.frame);
        if cel.file != expected {
            return Err(format!("refusing suspicious cel path '{}'", cel.file));
        }
    }
    if let Some(reference) = &meta.reference
        && reference != "reference.png"
    {
        return Err(format!(
            "stored reference must be 'reference.png', got '{reference}'"
        ));
    }
    Ok(())
}

fn structure_value(meta: &DocMeta, mut cel_keys: Vec<(usize, usize)>) -> Value {
    cel_keys.sort_unstable();
    let cels: Vec<Value> = cel_keys
        .into_iter()
        .map(|(layer, frame)| json!({"layer": layer, "frame": frame}))
        .collect();
    json!({
        "format_version": meta.format_version,
        "name": meta.name, "w": meta.w, "h": meta.h,
        "layers": meta.layers.iter().enumerate().map(|(index, layer)| json!({
            "index": index, "name": layer.name, "opacity": layer.opacity,
            "visible": layer.visible, "blend": layer.blend
        })).collect::<Vec<_>>(),
        "frames": meta.frames.iter().enumerate().map(|(index, frame)| json!({
            "index": index, "duration_ms": frame.duration_ms,
        })).collect::<Vec<_>>(),
        "tags": meta.tags.iter().map(|tag| json!({
            "name": tag.name, "from": tag.from, "to": tag.to, "direction": tag.direction
        })).collect::<Vec<_>>(),
        "cels": cels,
        "palette": meta.palette,
        "palette_len": meta.palette.len(),
        "reference": meta.reference,
    })
}

/// True for the `L<layer>_F<frame>.png` shape `save` writes — the only files
/// the stale-cel sweep may remove.
fn is_cel_filename(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('L').and_then(|s| s.strip_suffix(".png")) else {
        return false;
    };
    let Some((l, f)) = rest.split_once("_F") else {
        return false;
    };
    !l.is_empty()
        && !f.is_empty()
        && l.chars().all(|c| c.is_ascii_digit())
        && f.chars().all(|c| c.is_ascii_digit())
}

/// How `snap_to_palette` treats the partial-alpha pixels that continuous-tone FX
/// (blur/gradient/drop_shadow) and AA fringes leave behind — the difference
/// between "snap the colour but keep 200 soft alphas off-palette" and "make it
/// crisp pixel art again".
#[derive(Clone, Copy, Debug)]
pub enum AlphaSnap {
    /// Keep each pixel's source alpha; only the RGB is snapped, preserving
    /// deliberate soft edges.
    Preserve,
    /// Binarise alpha at `cutoff`: a pixel with alpha ≥ cutoff becomes fully
    /// opaque and snaps to the palette; below cutoff it is cleared. Collapses a
    /// bloom/AA gradient into a single crisp on-palette silhouette.
    Opaque(u8),
    /// Composite each pixel over `bg` (straight-alpha source-over) and snap the
    /// resulting opaque colour — flattens soft FX onto a known backdrop colour.
    Flatten([u8; 4]),
}

/// New index of a layer after moving the element at `from` to `to` (the same
/// remove-then-insert the `Vec` does). Used to keep cel keys in step with
/// `move_layer`.
fn remap_move(old: usize, from: usize, to: usize) -> usize {
    if old == from {
        return to;
    }
    let mut i = if old > from { old - 1 } else { old };
    if i >= to {
        i += 1;
    }
    i
}

/// Default per-frame duration for a freshly created frame (milliseconds).
pub const DEFAULT_FRAME_MS: u32 = 100;

impl AnalysisDocument {
    /// Validate a document's metadata, reference, cel container, and every cel
    /// header without decoding pixel payloads. Used by structure-only reads.
    pub fn load_structure(dir: &Path) -> Result<AnalysisDocument, String> {
        let meta = Document::load_metadata(dir)?;
        let cels = load_stored_cels(dir, &meta, |_| false)?;
        Ok(AnalysisDocument {
            meta,
            cels,
            frames: HashSet::new(),
            layer: None,
        })
    }

    /// Load only the cels needed to analyze `frames` and, when present, one
    /// `layer`. Frame and layer indices are validated against the complete
    /// metadata before any cel is decoded.
    pub fn load(
        dir: &Path,
        frames: &[usize],
        layer: Option<usize>,
    ) -> Result<AnalysisDocument, String> {
        let meta = Document::load_metadata(dir)?;
        if frames.is_empty() {
            return Err("analysis requires at least one frame".into());
        }
        if frames.len() > MAX_DOCUMENT_FRAMES {
            return Err(format!(
                "analysis requested {} frame entries; limit is {MAX_DOCUMENT_FRAMES}",
                frames.len()
            ));
        }
        for &frame in frames {
            if frame >= meta.frames.len() {
                return Err(format!("no frame {frame} (frames={})", meta.frames.len()));
            }
        }
        let frames: HashSet<usize> = frames.iter().copied().collect();
        if let Some(layer) = layer
            && layer >= meta.layers.len()
        {
            return Err(format!("no layer {layer}"));
        }
        let cels = load_stored_cels(dir, &meta, |cel| {
            frames.contains(&cel.frame) && layer.is_none_or(|layer| cel.layer == layer)
        })?;
        Ok(AnalysisDocument {
            meta,
            cels,
            frames,
            layer,
        })
    }

    /// Read-only view of the complete document metadata.
    pub fn meta(&self) -> &DocMeta {
        &self.meta
    }

    /// JSON document structure derived from the complete validated metadata.
    pub fn structure(&self) -> Value {
        structure_value(
            &self.meta,
            self.meta
                .cels
                .iter()
                .map(|cel| (cel.layer, cel.frame))
                .collect(),
        )
    }
}

impl Document {
    /// Read-only view of the document metadata. The field itself is
    /// crate-private: meta and the cel map move in lock-step (layer/frame
    /// reindexing), so outside the crate every mutation goes through a method
    /// that preserves that invariant.
    pub fn meta(&self) -> &DocMeta {
        &self.meta
    }

    /// Set or clear the stored reference-image file name, returning the
    /// previous one (so a caller can delete the replaced file). The one
    /// meta field external callers may write — it has no cel coupling.
    pub fn set_reference_file(&mut self, name: Option<String>) -> Option<String> {
        std::mem::replace(&mut self.meta.reference, name)
    }

    pub fn new(name: &str, w: u32, h: u32) -> Document {
        let meta = DocMeta {
            format_version: DOCUMENT_FORMAT_VERSION,
            name: name.to_string(),
            w,
            h,
            palette: Vec::new(),
            layers: vec![LayerMeta {
                name: "Layer 1".into(),
                opacity: 255,
                visible: true,
                blend: raster::Blend::Normal,
            }],
            frames: vec![FrameMeta {
                duration_ms: DEFAULT_FRAME_MS,
            }],
            tags: Vec::new(),
            cels: Vec::new(),
            reference: None,
        };
        Document {
            meta,
            cels: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    /// Load and validate only `doc.json`, without decoding cel images.
    ///
    /// Listing and inspection paths use this to share the same bounded,
    /// symlink-refusing persistence contract as a full document open.
    pub fn load_metadata(dir: &Path) -> Result<DocMeta, String> {
        let directory = std::fs::symlink_metadata(dir)
            .map_err(|error| format!("cannot inspect document '{}': {error}", dir.display()))?;
        if !directory.file_type().is_dir() {
            return Err(format!(
                "document '{}' must be a directory (symlinks are refused)",
                dir.display()
            ));
        }

        let s = read_regular_utf8_bounded(
            &dir.join("doc.json"),
            "document metadata",
            MAX_DOCUMENT_METADATA_BYTES,
        )?;
        let meta: DocMeta = serde_json::from_str(&s).map_err(|e| e.to_string())?;
        meta.validate()?;
        Ok(meta)
    }

    pub fn load(dir: &Path) -> Result<Document, String> {
        let meta = Self::load_metadata(dir)?;
        // Probe every cel and enforce the document-wide decoded-pixel budget
        // before decoding even the first image. A document with many
        // individually valid 4096px cels must not grow memory incrementally and
        // fail only after hundreds of MiB have already been retained.
        let cels = load_stored_cels(dir, &meta, |_| true)?;
        // Freshly loaded cels match their files — nothing is dirty yet.
        Ok(Document {
            meta,
            cels,
            dirty: HashSet::new(),
        })
    }

    pub fn save(&mut self, dir: &Path) -> Result<(), String> {
        // Reconcile and validate the complete in-memory generation before any
        // directory creation or image write. Transaction callers can then rely
        // on a rejected oversized document leaving their staged tree untouched.
        if self.cels.len() > MAX_DOCUMENT_CELS {
            return Err(format!(
                "document has {} cels; limit is {MAX_DOCUMENT_CELS}",
                self.cels.len()
            ));
        }
        let mut cel_metas = Vec::with_capacity(self.cels.len());
        for ((layer, frame), (x, y, img)) in &self.cels {
            raster::checked_rgba_dimensions(
                "cel",
                u64::from(img.width()),
                u64::from(img.height()),
            )?;
            cel_metas.push(CelMeta {
                layer: *layer,
                frame: *frame,
                x: *x,
                y: *y,
                file: cel_file(*layer, *frame),
            });
        }
        checked_cel_pixel_total(
            self.cels
                .values()
                .map(|(_, _, img)| (u64::from(img.width()), u64::from(img.height()))),
        )?;
        cel_metas.sort_by_key(|c| (c.layer, c.frame));
        self.meta.cels = cel_metas;
        self.meta.validate()?;
        let metadata_source =
            serde_json::to_string_pretty(&self.meta).map_err(|e| e.to_string())?;
        if metadata_source.len() as u64 > MAX_DOCUMENT_METADATA_BYTES {
            return Err(format!(
                "serialized document metadata is {} bytes; limit is {MAX_DOCUMENT_METADATA_BYTES} bytes",
                metadata_source.len()
            ));
        }

        ensure_real_directory(dir, "document directory")?;
        ensure_real_directory(&dir.join("cels"), "stored cels directory")?;
        if let Some(reference) = &self.meta.reference {
            open_regular_file(&dir.join(reference), "stored reference")?;
        }
        for ((layer, frame), (_, _, img)) in &self.cels {
            let file = cel_file(*layer, *frame);
            // Write only cels dirtied since load (or whose file is missing) —
            // a one-pixel edit used to re-encode and rewrite every cel in the
            // document, which made large animated docs crawl.
            let stored_is_regular = std::fs::symlink_metadata(dir.join(&file))
                .is_ok_and(|metadata| metadata.file_type().is_file());
            if self.dirty.contains(&(*layer, *frame)) || !stored_is_regular {
                let path = dir.join(&file);
                let tmp = path.with_extension("png.tmp");
                let write = img
                    .save_with_format(&tmp, image::ImageFormat::Png)
                    .map_err(|e| e.to_string())
                    .and_then(|()| std::fs::rename(&tmp, &path).map_err(|e| e.to_string()));
                if let Err(error) = write {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(error);
                }
            }
        }
        // Atomic-ish structure write: temp file + same-dir rename, so a crash
        // mid-write leaves the previous doc.json intact instead of a torn one.
        let tmp = dir.join("doc.json.tmp");
        std::fs::write(&tmp, metadata_source).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, dir.join("doc.json")).map_err(|e| e.to_string())?;
        // Delete cel files no longer referenced (cleared cels, deleted layers/
        // frames) so the store doesn't accumulate orphans. Only the L<n>_F<n>.png
        // shape is eligible — anything else in the dir is the user's. Runs after
        // the doc.json rename: a crash can then only leave a harmless orphan,
        // never a structure that references a deleted file.
        let keep: HashSet<String> = self.meta.cels.iter().map(|c| c.file.clone()).collect();
        let rd = std::fs::read_dir(dir.join("cels")).map_err(|e| e.to_string())?;
        for entry in rd {
            let ent = entry.map_err(|e| e.to_string())?;
            let name = ent.file_name().to_string_lossy().into_owned();
            if is_cel_filename(&name) && !keep.contains(&format!("cels/{name}")) {
                std::fs::remove_file(ent.path()).map_err(|e| e.to_string())?;
            }
        }
        self.dirty.clear();
        Ok(())
    }

    /// Mark one cel for writing on the next `save`.
    fn mark_dirty(&mut self, layer: usize, frame: usize) {
        self.dirty.insert((layer, frame));
    }

    /// Re-keying ops (layer/frame remaps) rename every cel's file — all dirty.
    fn mark_all_dirty(&mut self) {
        self.dirty.extend(self.cels.keys().copied());
    }

    // -- structure ----------------------------------------------------------

    /// Append a new layer on top; returns its index.
    pub fn add_layer(&mut self, name: Option<String>, opacity: u8, blend: raster::Blend) -> usize {
        let idx = self.meta.layers.len();
        self.meta.layers.push(LayerMeta {
            name: name.unwrap_or_else(|| format!("Layer {}", idx + 1)),
            opacity,
            visible: true,
            blend,
        });
        idx
    }

    /// Patch a layer's visibility / opacity / blend (each optional).
    pub fn set_layer(
        &mut self,
        layer: usize,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<raster::Blend>,
    ) -> Result<(), String> {
        let l = self
            .meta
            .layers
            .get_mut(layer)
            .ok_or_else(|| format!("no layer {}", layer))?;
        if let Some(v) = visible {
            l.visible = v;
        }
        if let Some(o) = opacity {
            l.opacity = o;
        }
        if let Some(b) = blend {
            l.blend = b;
        }
        Ok(())
    }

    // -- layer lifecycle ----------------------------------------------------
    //
    // The layer stack is a `Vec<LayerMeta>`; cels are keyed by `(layer, frame)`.
    // Any structural change to the stack therefore has to re-key the cel map in
    // lock-step. `remap_cel_layers` is the single choke point for that so the
    // two never drift apart.

    /// Rebuild the cel map under a layer-index remapping. `map(old)` gives the
    /// new layer index, or `None` to drop that layer's cels. Frames are kept.
    fn remap_cel_layers<F: Fn(usize) -> Option<usize>>(&mut self, map: F) {
        let old = std::mem::take(&mut self.cels);
        for ((l, f), v) in old {
            if let Some(nl) = map(l) {
                self.cels.insert((nl, f), v);
            }
        }
        // Every surviving cel just changed its file name (L<old> → L<new>).
        self.mark_all_dirty();
    }

    /// Move layer `from` to index `to` (clamped), shifting the rest; cels follow.
    pub fn move_layer(&mut self, from: usize, to: usize) -> Result<(), String> {
        let n = self.meta.layers.len();
        if from >= n {
            return Err(format!("no layer {} (layers={})", from, n));
        }
        let to = to.min(n - 1);
        if from == to {
            return Ok(());
        }
        let lm = self.meta.layers.remove(from);
        self.meta.layers.insert(to, lm);
        self.remap_cel_layers(|old| Some(remap_move(old, from, to)));
        Ok(())
    }

    /// Insert a new empty layer at `index` (clamped to the stack length),
    /// shifting existing layers (and their cels) up. Returns the new index.
    pub fn insert_layer(
        &mut self,
        index: usize,
        name: Option<String>,
        opacity: u8,
        blend: raster::Blend,
    ) -> usize {
        let n = self.meta.layers.len();
        let index = index.min(n);
        self.meta.layers.insert(
            index,
            LayerMeta {
                name: name.unwrap_or_else(|| format!("Layer {}", n + 1)),
                opacity,
                visible: true,
                blend,
            },
        );
        self.remap_cel_layers(|old| Some(if old >= index { old + 1 } else { old }));
        index
    }

    /// Delete a layer and its cels (cannot remove the last remaining layer).
    pub fn delete_layer(&mut self, index: usize) -> Result<(), String> {
        let n = self.meta.layers.len();
        if index >= n {
            return Err(format!("no layer {} (layers={})", index, n));
        }
        if n == 1 {
            return Err("cannot delete the only layer".into());
        }
        self.meta.layers.remove(index);
        self.remap_cel_layers(|old| match old.cmp(&index) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(old - 1),
            std::cmp::Ordering::Less => Some(old),
        });
        Ok(())
    }

    /// Rename a layer.
    pub fn rename_layer(&mut self, index: usize, name: String) -> Result<(), String> {
        let l = self
            .meta
            .layers
            .get_mut(index)
            .ok_or_else(|| format!("no layer {}", index))?;
        l.name = name;
        Ok(())
    }

    /// Duplicate a layer (meta + cels) directly above it. Returns the new index.
    pub fn duplicate_layer(&mut self, index: usize) -> Result<usize, String> {
        let n = self.meta.layers.len();
        if index >= n {
            return Err(format!("no layer {} (layers={})", index, n));
        }
        let mut lm = self.meta.layers[index].clone();
        lm.name = format!("{} copy", lm.name);
        let new_index = index + 1;
        self.meta.layers.insert(new_index, lm);
        // Shift cels at/above the insertion point up; the source (at `index <
        // new_index`) is untouched, so we can then copy it into `new_index`.
        self.remap_cel_layers(|old| Some(if old >= new_index { old + 1 } else { old }));
        let src: Vec<(usize, (i32, i32, RgbaImage))> = self
            .cels
            .iter()
            .filter(|((l, _), _)| *l == index)
            .map(|((_, f), v)| (*f, (v.0, v.1, v.2.clone())))
            .collect();
        for (f, v) in src {
            self.cels.insert((new_index, f), v);
            self.mark_dirty(new_index, f);
        }
        Ok(new_index)
    }

    /// Merge layer `index` down onto the layer below it, baking in the upper
    /// layer's opacity and blend mode (per frame), then remove the upper layer.
    /// The upper layer's pixels composite even when it is invisible — merging
    /// is a structural bake, not a render of the visible stack.
    pub fn merge_down(&mut self, index: usize) -> Result<(), String> {
        let n = self.meta.layers.len();
        if index >= n {
            return Err(format!("no layer {} (layers={})", index, n));
        }
        if index == 0 {
            return Err("layer 0 has nothing below it to merge into".into());
        }
        let lower = index - 1;
        let upper = self.meta.layers[index].clone();
        let blend = upper.blend;
        for f in 0..self.meta.frames.len() {
            if !self.cels.contains_key(&(index, f)) {
                continue;
            }
            let upper_img = self.cel_full(index, f);
            let lower_canvas = self.cel_canvas(lower, f)?;
            raster::composite(lower_canvas, &upper_img, 0, 0, upper.opacity, blend);
        }
        self.delete_layer(index)
    }

    /// Shift cel frame indices: every cel on a frame `>= from` moves by
    /// `delta` frames. The frame-axis twin of `remap_cel_layers` — the single
    /// choke point for keeping the cel map in lock-step with frame inserts,
    /// deletes and tween expansion.
    fn shift_cel_frames(&mut self, from: usize, delta: isize) {
        let keys: Vec<(usize, usize)> = self.cels.keys().filter(|k| k.1 >= from).cloned().collect();
        let mut moved = Vec::new();
        for k in keys {
            let v = self.cels.remove(&k).unwrap();
            moved.push(((k.0, (k.1 as isize + delta) as usize), v));
        }
        self.cels.extend(moved);
        // Re-keyed cels (F<old> → F<new>) all need writing under the new name.
        self.mark_all_dirty();
    }

    /// Append a new frame; with `copy_from`, duplicate that frame's cels into it.
    pub fn add_frame(&mut self, duration_ms: u32, copy_from: Option<usize>) -> usize {
        let idx = self.meta.frames.len();
        self.meta.frames.push(FrameMeta { duration_ms });
        if let Some(src) = copy_from {
            // duplicate every cel of frame `src` into the new frame
            let to_copy: Vec<(usize, (i32, i32, RgbaImage))> = self
                .cels
                .iter()
                .filter(|((_, f), _)| *f == src)
                .map(|((l, _), v)| (*l, (v.0, v.1, v.2.clone())))
                .collect();
            for (l, v) in to_copy {
                self.cels.insert((l, idx), v);
                self.mark_dirty(l, idx);
            }
        }
        idx
    }

    /// Add a named animation tag over an inclusive frame range.
    pub fn add_tag(
        &mut self,
        name: &str,
        from: usize,
        to: usize,
        direction: TagDirection,
    ) -> Result<(), String> {
        if from > to || to >= self.meta.frames.len() {
            return Err(format!(
                "tag range {}..{} out of bounds (frames={})",
                from,
                to,
                self.meta.frames.len()
            ));
        }
        self.meta.tags.push(TagMeta {
            name: name.into(),
            from,
            to,
            direction,
        });
        Ok(())
    }

    fn check_cel(&self, layer: usize, frame: usize) -> Result<(), String> {
        if layer >= self.meta.layers.len() {
            return Err(format!("no layer {}", layer));
        }
        if frame >= self.meta.frames.len() {
            return Err(format!("no frame {}", frame));
        }
        Ok(())
    }

    /// Place (or replace) the cel image for (layer, frame) at offset (x, y).
    pub fn set_cel(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        img: RgbaImage,
    ) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        self.cels.insert((layer, frame), (x, y, img));
        self.mark_dirty(layer, frame);
        Ok(())
    }

    /// Remove the cel at (layer, frame), if any.
    ///
    /// Validates the target like every sibling cel op: without this,
    /// `clear_cel(99, 0)` on a one-layer document reported success, so an agent
    /// clearing the wrong index was told the cel was empty and carried on.
    pub fn clear_cel(&mut self, layer: usize, frame: usize) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        self.cels.remove(&(layer, frame));
        // Not dirty (nothing to write) — its old file goes stale, which `save`
        // sweeps after the doc.json rename.
        self.dirty.remove(&(layer, frame));
        Ok(())
    }

    /// JSON snapshot of the document structure (layers, frames, tags, cels,
    /// palette) for inspection — no pixel data.
    pub fn structure(&self) -> Value {
        structure_value(&self.meta, self.cels.keys().copied().collect())
    }

    /// Test-only view of the live cel keys — the meta↔cel lock-step invariant
    /// the structural fuzzer asserts after every op.
    #[cfg(test)]
    pub(crate) fn cel_keys(&self) -> Vec<(usize, usize)> {
        self.cels.keys().copied().collect()
    }

    /// Read one pixel from a cel (document coords). Returns RGBA; out-of-bounds
    /// or an empty cel reads as transparent `[0,0,0,0]`. Read-only — never
    /// materialises a blank cel (unlike `cel_canvas`).
    pub fn get_pixel(&self, layer: usize, frame: usize, x: i32, y: i32) -> Result<[u8; 4], String> {
        self.check_cel(layer, frame)?;
        let transparent = [0, 0, 0, 0];
        let Some((cx, cy, img)) = self.cels.get(&(layer, frame)) else {
            return Ok(transparent);
        };
        let (lx, ly) = (x - cx, y - cy);
        if lx < 0 || ly < 0 || lx as u32 >= img.width() || ly as u32 >= img.height() {
            return Ok(transparent);
        }
        Ok(img.get_pixel(lx as u32, ly as u32).0)
    }

    // -- per-pixel drawing --------------------------------------------------

    /// Get the cel as a full-canvas image anchored at (0,0), creating/normalising
    /// it if needed so drawing coordinates equal document pixel coordinates.
    fn cel_canvas(&mut self, layer: usize, frame: usize) -> Result<&mut RgbaImage, String> {
        self.check_cel(layer, frame)?;
        let (w, h) = (self.meta.w, self.meta.h);
        let key = (layer, frame);
        let needs = match self.cels.get(&key) {
            Some((x, y, img)) => *x != 0 || *y != 0 || img.width() != w || img.height() != h,
            None => true,
        };
        if needs {
            let mut full = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
            if let Some((x, y, img)) = self.cels.get(&key) {
                for yy in 0..img.height() {
                    for xx in 0..img.width() {
                        let p = img.get_pixel(xx, yy).0;
                        if p[3] > 0 {
                            let (tx, ty) = (*x + xx as i32, *y + yy as i32);
                            if tx >= 0 && ty >= 0 && (tx as u32) < w && (ty as u32) < h {
                                full.put_pixel(tx as u32, ty as u32, Rgba(p));
                            }
                        }
                    }
                }
            }
            self.cels.insert(key, (0, 0, full));
        }
        // Every caller of cel_canvas is a mutation op (draw/fx/region) — the
        // returned &mut image escapes our sight, so conservatively write it
        // back on the next save.
        self.mark_dirty(layer, frame);
        Ok(&mut self.cels.get_mut(&key).unwrap().2)
    }

    pub fn fill_cel(&mut self, layer: usize, frame: usize, color: [u8; 4]) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        let img = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba(color));
        self.cels.insert((layer, frame), (0, 0, img));
        self.mark_dirty(layer, frame);
        Ok(())
    }

    /// Full-canvas (0,0-anchored) copy of a cel, transparent where absent.
    /// Also the before/after snapshot for the studio's mutation-diff acks.
    pub fn cel_full(&self, layer: usize, frame: usize) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba([0, 0, 0, 0]));
        if let Some((cx, cy, src)) = self.cels.get(&(layer, frame)) {
            for y in 0..src.height() as i32 {
                for x in 0..src.width() as i32 {
                    let (tx, ty) = (cx + x, cy + y);
                    if tx >= 0 && ty >= 0 && (tx as u32) < self.meta.w && (ty as u32) < self.meta.h
                    {
                        img.put_pixel(tx as u32, ty as u32, *src.get_pixel(x as u32, y as u32));
                    }
                }
            }
        }
        img
    }

    /// Compare a full-canvas snapshot with the current cel without copying the
    /// current cel. Returns the changed-pixel count and its inclusive canvas
    /// bounding box.
    ///
    /// The current cel may be absent, offset, or a different size from the
    /// document canvas. Pixels outside it are compared as transparent, exactly
    /// as they are in [`Self::cel_full`].
    pub fn cel_change_summary(
        &self,
        layer: usize,
        frame: usize,
        before: &RgbaImage,
    ) -> Result<(u64, Option<[u32; 4]>), String> {
        self.check_cel(layer, frame)?;
        if before.dimensions() != (self.meta.w, self.meta.h) {
            return Err(format!(
                "cel snapshot is {}x{}; document canvas is {}x{}",
                before.width(),
                before.height(),
                self.meta.w,
                self.meta.h
            ));
        }

        let after = self.cels.get(&(layer, frame));
        let (mut changed, mut bbox): (u64, Option<[u32; 4]>) = (0, None);
        if let Some((0, 0, image)) = after
            && image.dimensions() == before.dimensions()
        {
            for (before_pixel, (x, y, after_pixel)) in before.pixels().zip(image.enumerate_pixels())
            {
                if before_pixel == after_pixel {
                    continue;
                }
                changed += 1;
                bbox = Some(match bbox {
                    None => [x, y, x, y],
                    Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                });
            }
            return Ok((changed, bbox));
        }

        for (x, y, before_pixel) in before.enumerate_pixels() {
            let after_pixel = after
                .and_then(|(cel_x, cel_y, image)| {
                    let local_x = i64::from(x) - i64::from(*cel_x);
                    let local_y = i64::from(y) - i64::from(*cel_y);
                    (local_x >= 0
                        && local_y >= 0
                        && local_x < i64::from(image.width())
                        && local_y < i64::from(image.height()))
                    .then(|| image.get_pixel(local_x as u32, local_y as u32))
                })
                .map_or([0, 0, 0, 0], |pixel| pixel.0);
            if before_pixel.0 == after_pixel {
                continue;
            }
            changed += 1;
            bbox = Some(match bbox {
                None => [x, y, x, y],
                Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
            });
        }
        Ok((changed, bbox))
    }

    /// Full-canvas RGBA image of one layer's cel at `frame` (anchored at 0,0,
    /// transparent where the cel is empty/outside). Read-only sibling of
    /// `flatten` for analysis tools that want a single layer instead of the
    /// composite. Out-of-range layer/frame → error.
    fn cel_image(&self, layer: usize, frame: usize) -> Result<RgbaImage, String> {
        render::cel_image_from(&self.meta, &self.cels, layer, frame)
    }
}
