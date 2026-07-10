//! atelier MCP server (rmcp). Exposes the headless document editor as MCP
//! tools over stdio or Streamable HTTP. Tools return JSON text content; studio
//! errors keep the {"error": ...} payload AND set `is_error` so MCP harnesses
//! flag the failure instead of treating it as a success.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, CreateMessageRequestParams, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, RawImageContent, RawResource,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, Role,
    SamplingMessage, SamplingMessageContent, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use atelier_studio::Studio;

fn j(v: Value) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
}

/// Wraps a studio result as a tool result: Ok becomes a JSON text part; errors
/// keep the {"error": ...} payload (replay and older clients sniff it) and set
/// `is_error` so the harness surfaces the failure.
fn res(r: Result<Value, String>) -> CallToolResult {
    match r {
        Ok(v) => CallToolResult::success(vec![Content::text(j(v))]),
        Err(e) => CallToolResult::error(vec![Content::text(j(json!({"error": e})))]),
    }
}

/// Wrap an image-producing studio result as MCP content: an inline PNG (so the
/// agent SEES the pixels in the same turn — no separate file read) plus a JSON
/// text part with the measured stats. Errors come back as a `{"error": ...}`
/// text part with `is_error` set, matching `res`.
fn img_result(r: Result<(Vec<u8>, Value), String>) -> CallToolResult {
    match r {
        Ok((png, report)) => CallToolResult::success(vec![
            Content::image(base64(&png), "image/png"),
            Content::text(j(report)),
        ]),
        Err(e) => CallToolResult::error(vec![Content::text(j(json!({"error": e})))]),
    }
}

/// Like `img_result`, but the PNG is optional: tools whose preview is
/// conditional (diff overlays, seam overlays) degrade to a plain text result.
fn opt_img_result(r: Result<(Option<Vec<u8>>, Value), String>) -> CallToolResult {
    match r {
        Ok((Some(png), report)) => img_result(Ok((png, report))),
        Ok((None, report)) => res(Ok(report)),
        Err(e) => res(Err(e)),
    }
}

/// Append a best-effort inline preview to a successful mutation: the op's JSON
/// report plus a PNG of the touched cel (or flattened frame), so the agent SEES
/// the result of an import/stamp/quantize in the same turn. A preview failure
/// never fails the call — the op already landed.
fn previewed(
    studio: &Studio,
    id: &str,
    layer: Option<usize>,
    frame: usize,
    r: Result<Value, String>,
) -> CallToolResult {
    match r {
        Ok(report) => opt_img_result(Ok((studio.preview_png(id, layer, frame).ok(), report))),
        Err(e) => res(Err(e)),
    }
}

/// A list of `[r,g,b(,a)]` arrays -> a palette of RGBA swatches.
fn palette_list(v: &[Vec<i64>]) -> Vec<[u8; 4]> {
    v.iter().map(|c| rgba(c)).collect()
}

/// [r,g,b] -> RGB (drops alpha) for light/tint colours.
fn rgb3(v: &[i64]) -> [u8; 3] {
    [
        v.first().copied().unwrap_or(0) as u8,
        v.get(1).copied().unwrap_or(0) as u8,
        v.get(2).copied().unwrap_or(0) as u8,
    ]
}

/// [r,g,b] or [r,g,b,a] -> RGBA (alpha defaults to 255).
fn rgba(v: &[i64]) -> [u8; 4] {
    [
        v.first().copied().unwrap_or(0) as u8,
        v.get(1).copied().unwrap_or(0) as u8,
        v.get(2).copied().unwrap_or(0) as u8,
        v.get(3).copied().unwrap_or(255) as u8,
    ]
}

/// Parse the `doc_snap_palette` / FX alpha policy string into an `AlphaSnap`.
fn alpha_snap(
    mode: Option<&str>,
    cutoff: Option<u8>,
    bg: Option<&[i64]>,
) -> atelier_core::document::AlphaSnap {
    use atelier_core::document::AlphaSnap;
    match mode.unwrap_or("preserve") {
        "opaque" => AlphaSnap::Opaque(cutoff.unwrap_or(128)),
        "flatten" => AlphaSnap::Flatten(bg.map(rgba).unwrap_or([0, 0, 0, 255])),
        _ => AlphaSnap::Preserve,
    }
}

/// Optional [x0,y0,x1,y1] -> (x0,y0,x1,y1); drops anything shorter than 4.
fn region(r: &Option<Vec<i32>>) -> Option<(i32, i32, i32, i32)> {
    r.as_ref()
        .filter(|r| r.len() >= 4)
        .map(|r| (r[0], r[1], r[2], r[3]))
}

// --- library params --------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct DocCreate {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRef {
    pub doc_id: String,
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListDocs {
    /// Keep documents whose id starts with this (family selector, e.g. "hero-").
    pub prefix: Option<String>,
    /// Keep documents whose id contains this substring.
    pub contains: Option<String>,
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct DocSetAudit {
    /// Explicit member document ids (combined with `prefix` as a union).
    pub ids: Option<Vec<String>>,
    /// Select every document whose id starts with this (e.g. "hero-").
    pub prefix: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSetPaletteSync {
    /// Explicit target document ids (combined with `prefix` as a union).
    pub ids: Option<Vec<String>>,
    /// Select every document whose id starts with this (e.g. "hero-").
    pub prefix: Option<String>,
    /// The palette to broadcast, as [[r,g,b(,a)],...]. Or use `from_doc`.
    pub palette: Option<Vec<Vec<i64>>>,
    /// Copy the locked palette from this document instead of passing colours.
    pub from_doc: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ExportAll {
    pub target_dir: String,
    pub scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ExportAtlas {
    pub out_path: String,
    pub scale: Option<u32>,
    /// Max atlas width in pixels before the shelf packer wraps to a new row.
    pub max_width: Option<u32>,
}

// --- document params -------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct DocAddTag {
    pub doc_id: String,
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub direction: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocCel {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocStampImage {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub png_path: String,
    /// Scale factor (default 1.0). Shrinking uses area-average (features
    /// survive); growing uses nearest (stays crisp).
    pub scale: Option<f32>,
    /// Scale to this width in pixels instead (wins over `scale`) — e.g. stamp
    /// a 1024px reference at 32px wide without computing the factor.
    pub target_w: Option<u32>,
    /// Rotation in degrees, clockwise (default 0).
    pub rotate: Option<f32>,
    /// Opacity 0..255 when compositing (default 255).
    pub opacity: Option<u8>,
    /// Blend mode when compositing (default "normal").
    pub blend: Option<String>,
    /// true overwrites the whole cel; false (default) composites OVER it.
    pub replace: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocWangTiles {
    pub doc_id: String,
    /// Tile size N in pixels; the source canvas must be at least N×N.
    pub n: u32,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocNineSlice {
    pub doc_id: String,
    /// Destination layer / frame.
    pub layer: usize,
    pub frame: usize,
    /// Source panel layer / frame (defaults: same layer, frame 0).
    pub src_layer: Option<usize>,
    pub src_frame: Option<usize>,
    /// Source panel rect [x, y, w, h] — the panel authored once.
    pub src: Vec<i64>,
    /// Corner thickness in px (the 3×3 cut; default 3).
    pub inset: Option<i64>,
    /// Destination rect [x, y, w, h] — any size ≥ the corners.
    pub dst: Vec<i64>,
    /// How edges/centre fill: "tile" (default) or "stretch".
    pub mode: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocEmit {
    pub doc_id: String,
    pub layer: usize,
    /// Emitter rect [x, y, w, h] particles spawn inside.
    pub region: Vec<i64>,
    /// Frame count (default 8, clamped 2..=24).
    pub frames: Option<usize>,
    /// Particle count (default 24, clamped 1..=512).
    pub count: Option<usize>,
    /// Emission direction in degrees (0=right, 90=down, 270=up; default 270).
    pub angle: Option<f32>,
    /// Half-cone spread around `angle` in degrees (default 30).
    pub spread: Option<f32>,
    /// Initial speed in px/frame (default 1.5).
    pub speed: Option<f32>,
    /// Downward acceleration in px/frame² (negative = rises; default 0).
    pub gravity: Option<f32>,
    /// Particle lifetime in loop units — 1.0 = lives one full cycle (default 1).
    pub life: Option<f32>,
    /// Particle size in px at birth (shrinks as it dies; default 2).
    pub size: Option<i64>,
    /// Deterministic seed (default 1).
    pub seed: Option<u64>,
    /// Base colour [r,g,b(,a)] — auto-ramped unless `ramp` given.
    pub color: Vec<i64>,
    /// Explicit birth→death colour ramp [[r,g,b,a],...].
    pub ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocColorblindCheck {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// Nearest-neighbour upscale of the returned strip (default 2, clamped 1..=8).
    pub scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocAutotileSet {
    pub doc_id: String,
    /// Tile size N in pixels; the source canvas must be at least N×N.
    pub n: u32,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocTilemapAssemble {
    pub doc_id: String,
    /// Tile size N in pixels; the source canvas must be at least N×N.
    pub n: u32,
    /// The terrain mask, one string per map row: `#`/`1`/`x` = filled cell,
    /// anything else = empty. Short rows pad with empty.
    pub rows: Vec<String>,
    /// How off-map cells read: "filled" (terrain continues past the edge,
    /// default) or "empty" (map borders get outlines).
    pub outside: Option<String>,
}

// --- drawing params --------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct DocTween {
    pub doc_id: String,
    pub from: usize,
    pub to: usize,
    /// In-between frames to insert (default 1).
    pub steps: Option<usize>,
    /// Duration of each inserted frame in ms (default 100).
    pub duration_ms: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocGlow {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Glow tint; omit to bloom the art's own colours.
    pub color: Option<Vec<i64>>,
    /// Blur radius (default 2).
    pub radius: Option<i32>,
    /// Glow strength 0..255 (default 180).
    pub intensity: Option<u8>,
    /// Blend mode for the bloom (default "screen"; "add" for hotter).
    pub mode: Option<String>,
    /// When a palette is locked, re-snap the bloom back ON-palette afterwards so
    /// it stays crisp pixel art instead of hundreds of soft off-palette tints
    /// (the bloom is binarised at `snap_cutoff`). Default true when a palette is
    /// set; pass false to keep a deliberately soft bloom.
    pub snap: Option<bool>,
    /// Alpha cutoff for the post-glow snap (0..255, default 64 — keeps the
    /// brighter bloom core, drops the faint halo).
    pub snap_cutoff: Option<u8>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRimLight {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Rim colour [r,g,b(,a)].
    pub color: Vec<i64>,
    /// Light azimuth in degrees: 0=right, 90=down, 180=left, 270=up.
    pub az: f32,
    /// Rim band thickness in pixels (default 1).
    pub width: Option<i32>,
    /// Facing falloff exponent — higher = tighter rim (default 1.5).
    pub falloff: Option<f32>,
    /// Light the AWAY-facing edge instead (core/contact shadow). Default false.
    pub dark: Option<bool>,
    /// Snap on-palette afterwards when a palette is locked (default true).
    pub snap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocCastShadow {
    pub doc_id: String,
    /// The caster layer (its silhouette throws the shadow).
    pub layer: usize,
    pub frame: usize,
    /// Shadow colour [r,g,b(,a)].
    pub color: Vec<i64>,
    /// Light azimuth in degrees: 0=right, 90=down, 180=left, 270=up (pairs with
    /// the vector doc_form_audit infers). The shadow falls opposite. Default 135.
    pub az: Option<f32>,
    /// Ground stretch — how far the shadow projects (default 1.0).
    pub length: Option<f32>,
    /// How much height survives, 0..1 (0 = flat on the ground; default 0.2).
    pub squash: Option<f32>,
    /// Shadow strength 0..255 (default 140).
    pub opacity: Option<u8>,
    /// Paint the shadow onto this layer and clip it to that layer's opaque
    /// pixels (the ground surface). Omit = draw behind the caster on its own cel.
    pub receiver_layer: Option<usize>,
    /// Snap on-palette afterwards when a palette is locked (default true).
    pub snap: Option<bool>,
}

/// One gradient colour stop: position along the axis (0..1) + RGBA colour.
#[derive(Deserialize, JsonSchema)]
pub struct GradientStop {
    pub pos: f32,
    pub color: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSelect {
    pub doc_id: String,
    /// "rect" | "ellipse" | "color" | "all" | "none". Default "rect".
    pub shape: Option<String>,
    /// Combine with the current selection: "replace" (default) | "add" |
    /// "subtract" | "intersect".
    pub mode: Option<String>,
    /// rect shape:
    pub x0: Option<i32>,
    pub y0: Option<i32>,
    pub x1: Option<i32>,
    pub y1: Option<i32>,
    /// ellipse shape:
    pub cx: Option<i32>,
    pub cy: Option<i32>,
    pub rx: Option<i32>,
    pub ry: Option<i32>,
    /// color shape — which cel to test, and either an explicit `color` or a
    /// sample point (`x`,`y`) plus `tolerance` (max channel distance).
    pub layer: Option<usize>,
    pub frame: Option<usize>,
    pub color: Option<Vec<i64>>,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub tolerance: Option<i32>,
}

/// A rectangular region of a cel (inclusive corners) + optional offset.
#[derive(Deserialize, JsonSchema)]
pub struct DocSetPivot {
    pub doc_id: String,
    pub frame: usize,
    /// Anchor point [x,y] in document pixels; omit to clear the pivot.
    pub pivot: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct BoxInput {
    /// Label for this box (e.g. "torso", "sword", "feet").
    pub name: String,
    /// `body` (collision), `hit` (deals damage) or `hurt` (takes damage).
    pub kind: String,
    /// `[x, y, w, h]` in document pixels.
    pub rect: Vec<i32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSetFrameBoxes {
    pub doc_id: String,
    pub frame: usize,
    /// The frame's full box list; pass `[]` to clear. Replaces any existing set.
    pub boxes: Vec<BoxInput>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSetPalette {
    pub doc_id: String,
    /// Palette swatches, each [r,g,b] or [r,g,b,a].
    pub colors: Vec<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPaletteSwap {
    pub doc_id: String,
    /// Source colours to recolour, each [r,g,b]/[r,g,b,a]. Matched exactly
    /// (all channels). Same length as `to` — `from[i]` becomes `to[i]`.
    pub from: Vec<Vec<i64>>,
    /// Replacement colours, each [r,g,b]/[r,g,b,a]. Same length as `from`.
    pub to: Vec<Vec<i64>>,
    /// Restrict to one layer's cels; omit for every layer.
    pub layer: Option<usize>,
    /// Restrict to one frame's cels; omit for every frame.
    pub frame: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocBatch {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Ordered ops, each like {"op":"rect","x0":1,"y0":1,"x1":8,"y1":8,"color":[r,g,b],"fill":true}.
    /// Ops: pencil/line/rect/ellipse/polyline/polygon/bezier/fill/replace_color/
    /// flip/shift/outline/fill_cel/clear_cel/gradient/scatter/noise/adjust/blur/
    /// quantize/symmetry/drop_shadow/glow/bevel/shade/dither/pixel_perfect.
    pub ops: Vec<serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocDraw {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// One draw op: pencil | line | rect | ellipse | polyline | polygon | stroke
    /// | fill | gradient | scatter | noise | text | fill_cel.
    pub op: String,
    /// The op's own params, flattened alongside (e.g. for "rect": x0, y0, x1, y1,
    /// color, fill). Every op also accepts `opacity` and `blend_mode`.
    #[serde(flatten)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocFx {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// One transform/effect op: blur | outline | drop_shadow | bevel | shade |
    /// form | dither | pixel_perfect | flip | shift | symmetry | quantize |
    /// replace_color | adjust.
    pub op: String,
    /// The op's own params, flattened alongside. Every op also accepts `opacity`
    /// and `blend_mode`.
    #[serde(flatten)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocExport {
    pub doc_id: String,
    /// What to export: sheet | anim | tileset.
    pub op: String,
    /// Output file path.
    pub out_path: String,
    /// Nearest-neighbour upscale (sheet/anim default 4, tileset default 1).
    pub scale: Option<u32>,
    /// Op-specific params, flattened: anim → format ("gif"|"apng"), tag;
    /// tileset → tile_w, tile_h.
    #[serde(flatten)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocLayer {
    pub doc_id: String,
    /// add (new layer on top) | set (visibility/opacity/blend of layer `index`) |
    /// move | insert | delete | rename | duplicate | merge_down.
    pub op: String,
    /// Target layer index (the layer for `set`/`move`/`delete`/…).
    pub index: Option<usize>,
    /// Destination index for `move`.
    pub to_index: Option<usize>,
    /// Layer name for `add`/`insert`/`rename`.
    pub name: Option<String>,
    /// Visibility for `set`.
    pub visible: Option<bool>,
    pub opacity: Option<u8>,
    pub blend: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocFrame {
    pub doc_id: String,
    /// add (append) | duration (set frame timing) | delete | insert | duplicate |
    /// move.
    pub op: String,
    /// Target frame index (for duration/delete/insert/duplicate/move).
    pub frame: Option<usize>,
    /// For `add`: duplicate this frame's cels into the new frame.
    pub copy_from: Option<usize>,
    /// Destination index for `move`.
    pub to_index: Option<usize>,
    /// Frame duration in ms (for add/insert/duration; default 100).
    pub duration_ms: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRegion {
    pub doc_id: String,
    /// copy | cut | clear | move | paste.
    pub op: String,
    pub layer: usize,
    pub frame: usize,
    /// Source rect for copy/cut/clear/move.
    pub x0: Option<i32>,
    pub y0: Option<i32>,
    pub x1: Option<i32>,
    pub y1: Option<i32>,
    /// Offset for `move`.
    pub dx: Option<i32>,
    pub dy: Option<i32>,
    /// Destination top-left for `paste`.
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// `paste`: true = source-over (default), false = overwrite.
    pub blend: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRefOp {
    pub doc_id: String,
    /// set (attach/clear the comparison reference) | import (trace an external
    /// image cleaned onto a guide layer).
    pub op: String,
    /// `set`: reference image path (omit to clear). `import`: source image path.
    pub path: Option<String>,
    // -- import params --
    pub layer: Option<usize>,
    pub frame: Option<usize>,
    /// `import`: target width in px (required for import).
    pub target_w: Option<u32>,
    /// `import`: omit to derive an aspect-true height.
    pub target_h: Option<u32>,
    pub colors: Option<usize>,
    pub dither: Option<bool>,
    pub defringe: Option<bool>,
    pub to_doc_palette: Option<bool>,
    /// `import`: corner-seeded background removal before palette extraction.
    pub remove_bg: Option<bool>,
    /// `import`: colours the derived palette must keep (e.g. a black outline).
    pub pin: Option<Vec<Vec<i64>>>,
}

// --- canvas reader params --------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct DocDumpRegion {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// Dump this layer's cel; omit to dump the flattened composite.
    pub layer: Option<usize>,
    /// [x0,y0,x1,y1] document pixels (inclusive). Omit = whole canvas. Area capped at 4096 px.
    pub region: Option<Vec<i32>>,
    /// "symbol" (A..Z a..z 0..9 per colour, `.`=transparent) or "hex".
    pub mode: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSilhouette {
    pub doc_id: String,
    pub frame: Option<usize>,
    pub layer: Option<usize>,
    /// Minimum alpha counted as opaque (default 1).
    pub alpha_threshold: Option<u8>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocComponents {
    pub doc_id: String,
    pub frame: Option<usize>,
    pub layer: Option<usize>,
    /// Pixel adjacency: 4 or 8 (default 8).
    pub connectivity: Option<u8>,
    /// Only components of this exact [r,g,b]/[r,g,b,a]; omit = any opaque pixel.
    pub color: Option<Vec<i64>>,
    /// Components smaller than this are dropped from the list (default 1).
    pub min_area: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocFormAudit {
    pub doc_id: String,
    pub frame: Option<usize>,
    pub layer: Option<usize>,
    /// Forms smaller than this many pixels are skipped (default 12).
    pub min_area: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocCoverageMap {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// Grid columns (default 8).
    pub cols: Option<u32>,
    /// Grid rows (default 8).
    pub rows: Option<u32>,
}

// --- value & colour feedback params ----------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct DocContrastCheck {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// "region" (inside vs 4px surround), "palette" (all colour pairs) or "one-bit".
    pub mode: String,
    /// [x0,y0,x1,y1] document pixels — required for mode="region".
    pub region: Option<Vec<i32>>,
    /// WCAG ratio at/above which a pair passes (default 1.5).
    pub min_ratio: Option<f32>,
    /// Luma cutoff for mode="one-bit" (default 128).
    pub threshold: Option<u8>,
    /// Where to write the B/W PNG for mode="one-bit".
    pub out_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPaletteReport {
    pub doc_id: String,
    /// One frame; omit to tally every frame.
    pub frame: Option<usize>,
    /// One layer's cel; omit for the flattened composite per frame.
    pub layer: Option<usize>,
    /// [x0,y0,x1,y1] document pixels to restrict the tally; omit = whole canvas.
    pub region: Option<Vec<i32>>,
    /// Max channel distance counting two colours as near-duplicates (default 8).
    pub dupe_threshold: Option<i32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRampValidate {
    /// Explicit ramp [[r,g,b],...] (≥2). Provide this OR `doc_id`.
    pub colors: Option<Vec<Vec<i64>>>,
    /// Validate this document's locked palette instead of explicit colours.
    pub doc_id: Option<String>,
    /// [start,end) slice of the palette to validate (with `doc_id`).
    pub slice: Option<Vec<usize>>,
}

// --- animation & tiling feedback params ------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct DocFrameDiff {
    pub doc_id: String,
    pub frame_a: usize,
    pub frame_b: usize,
    /// Diff this layer's cel; omit for the flattened composite.
    pub layer: Option<usize>,
    /// [x0,y0,x1,y1] document pixels; omit = whole canvas.
    pub region: Option<Vec<i32>>,
    /// Add a text grid (`.`unchanged `+`added `-`removed `~`recolored); area capped 4096 px.
    pub grid: Option<bool>,
    /// "none" or "overlay" (frame_b dimmed with changed pixels flagged).
    pub render: Option<String>,
    pub out_path: Option<String>,
    pub scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSeamReport {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// Test this layer's cel; omit for the flattened composite.
    pub layer: Option<usize>,
    /// "both", "horizontal" (left↔right) or "vertical" (top↔bottom).
    pub axis: Option<String>,
    /// Max per-channel delta still counted as a matching edge (default 0).
    pub threshold: Option<i32>,
    /// Render a PNG with mismatched edge pixels highlighted red; returns its path.
    pub out_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocAnimAudit {
    pub doc_id: String,
    /// Audit this tag's loop; omit to audit the whole timeline.
    pub tag: Option<String>,
    /// Use this layer's cel; omit for the flattened composite.
    pub layer: Option<usize>,
    /// "seam" (loop wrap diff), "spacing" (per-frame motion evenness),
    /// "arc" (trajectory shape) or "timing" (per-frame durations).
    pub mode: String,
    /// [x0,y0,x1,y1] clips spacing/arc to one part (e.g. a swinging arm) so
    /// its motion is measurable over a static body.
    pub region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocKeyframeMove {
    pub doc_id: String,
    pub layer: usize,
    /// Source rect [x0,y0,x1,y1] (document pixels) read from `from_frame`.
    pub region: Vec<i32>,
    pub from_frame: usize,
    /// Destination keyframe (> from_frame); all frames between move too.
    pub to_frame: usize,
    /// Total displacement applied at to_frame; intermediates are eased fractions.
    pub dx: i32,
    pub dy: i32,
    /// "linear", "ease-in", "ease-out", "ease-in-out" (cubic), "bounce"
    /// (ease-out bounce), "overshoot" (back ease-out — shoots past the offset
    /// then settles) or "elastic" (decaying oscillation). Default "linear".
    pub easing: Option<String>,
    /// Clear the original rect in each destination frame first (default true).
    pub clear_source: Option<bool>,
}

/// True when a tool result is a failure: either `is_error` is set, or its first
/// text content is a `{"error": ...}` payload (how `res` reports app errors).
/// The document id named in a tool result's top-level `id` field (doc_create
/// returns the new doc this way — its id isn't in the args). None for the common
/// case where the result carries no `id`.
fn result_doc_id(result: &rmcp::model::CallToolResult) -> Option<String> {
    let text = &result.content.first()?.as_text()?.text;
    let v: Value = serde_json::from_str(text).ok()?;
    v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn is_error_result(result: &rmcp::model::CallToolResult) -> bool {
    if result.is_error == Some(true) {
        return true;
    }
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .and_then(|t| serde_json::from_str::<Value>(&t.text).ok())
        .is_some_and(|v| v.get("error").is_some())
}

// --- session recorder ------------------------------------------------------

/// Records every tool call into a replayable recipe file (the inverse of
/// `atelier replay`): a good live session becomes a recipe for free. The shape
/// it writes matches `replay::Recipe` — `{name, description, steps:[{tool,args}]}`.
///
/// After each call it rewrites the whole file atomically (write `.tmp`, rename)
/// so a killed session still leaves a valid recipe up to the last completed step.
#[derive(Clone)]
struct Recorder {
    path: std::sync::Arc<std::path::PathBuf>,
    /// Accumulated steps, guarded so concurrent HTTP sessions append in order.
    steps: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Recorder {
    fn new(path: std::path::PathBuf) -> Self {
        // Create the recipe's parent dir once so `--record nested/dir/recipe.json`
        // works instead of every atomic write silently failing.
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "atelier: failed to create recording dir {}: {e}",
                    parent.display()
                );
            }
        }
        Self {
            path: std::sync::Arc::new(path),
            steps: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Append one `{tool, args}` step and rewrite the recipe file atomically. Only
    /// the caller's successful steps reach here (see `call_tool`), so the recipe
    /// stays replayable. Best-effort: a write failure is logged, never fails the call.
    fn record(&self, tool: &str, args: Value) {
        let mut steps = self.steps.lock().expect("recorder lock poisoned");
        steps.push(json!({"tool": tool, "args": args}));
        let name = self
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("session");
        let recipe = json!({
            "name": name,
            "description": format!("recorded session {}", iso_date()),
            "steps": &*steps,
        });
        // Pretty so the recipe stays hand-editable, like the shipped examples.
        let body = serde_json::to_string_pretty(&recipe).unwrap_or_else(|_| "{}".into());
        let tmp = self.path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &*self.path))
        {
            eprintln!(
                "atelier: failed to write recording {}: {e}",
                self.path.display()
            );
        }
    }
}

/// UTC calendar date as `YYYY-MM-DD`, computed from the wall clock without a
/// date-library dependency (civil-from-days, proleptic Gregorian).
fn iso_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// --- world-class-art tool params (the art-quality pass) --------------------

#[derive(Deserialize, JsonSchema)]
pub struct DocLook {
    pub doc_id: String,
    pub frame: Option<usize>,
    pub scale: Option<u32>,
    /// Inclusive crop corners [x0,y0,x1,y1].
    pub region: Option<Vec<i32>>,
    /// render | value | bands | sat | hue | notan.
    pub mode: Option<String>,
    /// Band count for mode=bands.
    pub bands: Option<u32>,
    pub grid: Option<bool>,
    pub coords: Option<bool>,
    pub onion: Option<bool>,
    pub max_size: Option<u32>,
    /// Repeat the result N×N to eyeball seamlessness (tileability check).
    pub tile: Option<u32>,
    /// Also write the PNG to this path, for file/export workflows.
    pub out_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSelectRender {
    pub doc_id: String,
    pub scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocCheckpoint {
    pub doc_id: String,
    /// save | list | restore | diff | prune.
    pub action: String,
    pub label: Option<String>,
    pub checkpoint_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSnapPalette {
    pub doc_id: String,
    pub layer: Option<usize>,
    pub frame: Option<usize>,
    /// Override palette as a list of [r,g,b(,a)]; defaults to the doc's palette.
    pub palette: Option<Vec<Vec<i64>>>,
    /// Partial-alpha policy for FX bloom / AA fringe: `preserve` (default — keep
    /// soft alpha, snap RGB only) | `opaque` (binarise alpha at `cutoff`,
    /// default 128 — collapse a bloom into a crisp on-palette silhouette) |
    /// `flatten` (composite over `bg` then snap fully opaque).
    pub alpha: Option<String>,
    /// Alpha cutoff for `alpha="opaque"` (0..255, default 128).
    pub cutoff: Option<u8>,
    /// Backdrop `[r,g,b]` for `alpha="flatten"` (default opaque black).
    pub bg: Option<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSelectWand {
    pub doc_id: String,
    /// Layer to sample; omit to sample the flattened composite.
    pub layer: Option<usize>,
    pub frame: Option<usize>,
    pub x: i32,
    pub y: i32,
    pub tol: Option<i32>,
    pub conn8: Option<bool>,
    pub perceptual: Option<bool>,
    /// replace | add | subtract | intersect.
    pub mode: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSmoothEdges {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Optional ramp [[r,g,b],...] to keep AA pixels on-palette.
    pub ramp: Option<Vec<Vec<i64>>>,
    /// With keep_square, edges flat-straight for longer than this stay crisp.
    pub max_run: Option<i32>,
    /// Preserve deliberate right-angle corners (default true).
    pub keep_square: Option<bool>,
    pub only_color: Option<Vec<i64>>,
    pub region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocTransformCel {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub region: Option<Vec<i32>>,
    pub rotate: Option<f32>,
    pub scale_x: Option<f32>,
    pub scale_y: Option<f32>,
    pub skew_x: Option<f32>,
    pub skew_y: Option<f32>,
    /// rotsprite (cluster-preserving) | nearest.
    pub method: Option<String>,
    /// With scale_x set and scale_y omitted, derive scale_y = 1/scale_x.
    pub preserve_volume: Option<bool>,
    /// Snap the resample fringe back to the locked palette (default true; only
    /// acts when a palette is set).
    pub snap_palette: Option<bool>,
    pub clear_source: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocCritique {
    pub doc_id: String,
    pub frame: Option<usize>,
    pub layer: Option<usize>,
    pub region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRelight {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub region: Option<Vec<i32>>,
    /// Key light: azimuth (0=right,90=down,180=left,270=up) + elevation (0..90).
    pub key_azimuth: Option<f32>,
    pub key_elevation: Option<f32>,
    pub key_intensity: Option<f32>,
    pub key_color: Option<Vec<i64>>,
    pub fill_intensity: Option<f32>,
    pub fill_color: Option<Vec<i64>>,
    pub rim_intensity: Option<f32>,
    pub rim_color: Option<Vec<i64>>,
    pub ambient: Option<f32>,
    pub ambient_color: Option<Vec<i64>>,
    /// How domed the silhouette reads as a form (higher = flatter).
    pub bulge: Option<f32>,
    pub ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocDitherRamp {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// The ramp to dither across (>= 2 colours), as [[r,g,b],...].
    pub ramp: Vec<Vec<i64>>,
    pub region: Option<Vec<i32>>,
    /// h | v | radial.
    pub axis: Option<String>,
    /// bayer2 | bayer4 | bayer8 | checker | ign.
    pub pattern: Option<String>,
    pub only_existing: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocContactSheet {
    pub doc_id: String,
    pub scale: Option<u32>,
    pub cols: Option<usize>,
    /// Ghost each cell's PREVIOUS frame under it at 35% alpha — per-pair
    /// onion skinning, the closest a still image gets to showing motion.
    pub onion: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPalette {
    pub base: Vec<i64>,
    /// mono | complementary | triadic | analogous | split | tetradic. Default mono.
    pub scheme: Option<String>,
    /// Colours per ramp (default 5).
    pub count: Option<usize>,
    pub value_lo: Option<f32>,
    pub value_hi: Option<f32>,
    pub hue_shift: Option<f32>,
    /// flat | arc | sat-in-shadow (default arc).
    pub sat_curve: Option<String>,
    pub anchor_midtone: Option<bool>,
    /// Store the flattened palette on this document id.
    pub set_doc: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocBox {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Centre of the top diamond.
    pub cx: i32,
    pub cy: i32,
    /// Half-width of the top diamond.
    pub s: i32,
    /// Body height.
    pub ht: i32,
    pub color: Vec<i64>,
    pub light_right: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPerspectiveGuide {
    pub doc_id: String,
    /// thirds | grid | iso | vp.
    pub kind: Option<String>,
    pub color: Option<Vec<i64>>,
    pub spacing: Option<i32>,
    /// Vanishing point [x,y] for kind=vp.
    pub vp: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocOutlineSelective {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// from_fill | light | dark.
    pub mode: Option<String>,
    pub ramp: Option<Vec<Vec<i64>>>,
    pub steps: Option<i32>,
    pub region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocMaterial {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub region: Option<Vec<i32>>,
    /// metal | wood | stone | water | cloth | skin | glass.
    pub material: String,
    pub color: Vec<i64>,
    pub seed: Option<u64>,
    pub ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocTranslucency {
    pub doc_id: String,
    pub frame: Option<usize>,
    pub layer: Option<usize>,
    pub region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPanel {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub fill: Vec<i64>,
    pub border: Option<Vec<i64>>,
    pub bevel: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPaintGrid {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Top-left of the grid in document pixels (default 0,0).
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// Single-character keys -> [r,g,b(,a)] colour OR an integer palette index
    /// (palette-true by construction). '.' and ' ' are reserved: untouched.
    pub legend: serde_json::Map<String, Value>,
    /// One string per pixel row, e.g. ["..kk..", ".koook."].
    pub rows: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocExtractToLayer {
    pub doc_id: String,
    /// Source layer holding the flat sprite.
    pub layer: usize,
    /// Frame to read when frames="one" (default 0).
    pub frame: Option<usize>,
    /// Rect [x0,y0,x1,y1] of the part to cut. Omit to use the active selection.
    pub region: Option<Vec<i32>>,
    /// Use the active selection mask (set with doc_select / doc_select_wand).
    pub use_selection: Option<bool>,
    /// Name for the new part layer, e.g. "arm-front".
    pub name: Option<String>,
    /// "one" (default) cuts just `frame`; "all" cuts every frame's cel.
    pub frames: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocKeyframeTransform {
    pub doc_id: String,
    pub layer: usize,
    /// Rect [x0,y0,x1,y1] of the part to rotate (document pixels), read from from_frame.
    pub region: Vec<i32>,
    /// The JOINT the part rotates about, [x,y] in document pixels (e.g. the shoulder).
    pub pivot: Vec<f32>,
    pub from_frame: usize,
    /// Last frame of the motion (> from_frame); frames must already exist.
    pub to_frame: usize,
    /// Total rotation in degrees at to_frame (clockwise positive); intermediates eased.
    pub rot_deg: Option<f32>,
    /// Total translation applied at to_frame; intermediates eased.
    pub dx: Option<i32>,
    pub dy: Option<i32>,
    /// linear | ease-in | ease-out | ease-in-out | bounce | overshoot | elastic.
    pub easing: Option<String>,
    /// Snap resampled pixels back to the locked palette (default true).
    pub snap_palette: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRefAnalyze {
    pub doc_id: String,
    /// Analyze this file instead of the doc's stored reference.
    pub path: Option<String>,
    /// Width to plan at (default: the canvas width); height derives aspect-true.
    pub target_w: Option<u32>,
    /// Subject palette size to extract (default 8).
    pub colors: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRefCompare {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// "side_by_side" (default) or "overlay" (reference ghosted under the art).
    pub mode: Option<String>,
    /// Grid divisions per axis for the ΔE cells (default 8, clamped 2..=16).
    pub cells: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocDiffMap {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// How many worst individual pixels to list (default 20, clamped 1..=64).
    pub top: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocCritiqueVision {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// What to weight the critique toward — e.g. "anatomy", "readability",
    /// "colour", "appeal". Omit for a balanced pass.
    pub focus: Option<String>,
    /// Render scale handed to the host's vision model (default 8, clamped
    /// 1..=16). Bigger = more pixel detail for the model to read.
    pub scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocBurst {
    pub doc_id: String,
    pub layer: Option<usize>,
    pub cx: i32,
    pub cy: i32,
    pub frames: Option<usize>,
    pub max_radius: i32,
    /// ring | disc | rays.
    pub kind: Option<String>,
    pub color: Vec<i64>,
    pub ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocFigure {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Named joint coordinates as `{"head":[x,y], "shoulder_l":[x,y], ...}`.
    /// Required: head, shoulder_l/r, elbow_l/r, hand_l/r, hip_l/r, knee_l/r,
    /// foot_l/r. chest and pelvis are derived from the shoulder/hip midpoints.
    pub joints: std::collections::HashMap<String, Vec<i64>>,
    pub color: Vec<i64>,
    /// Arm/leg capsule width (default 3).
    pub limb_w: Option<i64>,
    /// Torso capsule width (default 6).
    pub torso_w: Option<i64>,
    /// Head radius (default 4).
    pub head_r: Option<i64>,
    /// Anti-alias the capsule edges (default true).
    pub aa: Option<bool>,
    /// Snap on-palette afterwards when a palette is locked (default true).
    pub snap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPoseCycle {
    pub doc_id: String,
    pub layer: usize,
    /// The base STANDING pose: the same 13 named joints as doc_figure.
    pub joints: std::collections::HashMap<String, Vec<i64>>,
    /// Which cycle to generate: idle | run | jump | attack | hurt.
    pub gait: String,
    /// Frame count (omit or 0 = the gait's natural count; clamped 2..=24).
    pub frames: Option<usize>,
    /// Amplitude multiplier on the gait's motion (default 1.0, clamped 0.1..=3).
    pub intensity: Option<f32>,
    pub color: Vec<i64>,
    pub limb_w: Option<i64>,
    pub torso_w: Option<i64>,
    pub head_r: Option<i64>,
    pub aa: Option<bool>,
    pub snap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocWalk {
    pub doc_id: String,
    pub layer: usize,
    /// The base STANDING pose: the same 13 named joints as doc_figure.
    pub joints: std::collections::HashMap<String, Vec<i64>>,
    pub color: Vec<i64>,
    /// Number of frames in the cycle (default 8, clamped 2..=24).
    pub frames: Option<usize>,
    /// Foot front-to-back travel in px (default 10).
    pub stride: Option<i64>,
    /// Foot lift height on the swing in px (default 4).
    pub lift: Option<i64>,
    /// Body bob amplitude in px (default 1).
    pub bob: Option<i64>,
    /// Hand front-to-back swing in px (default 6).
    pub arm_swing: Option<i64>,
    pub limb_w: Option<i64>,
    pub torso_w: Option<i64>,
    pub head_r: Option<i64>,
    pub aa: Option<bool>,
    pub snap: Option<bool>,
}

// --- resources -------------------------------------------------------------

/// Scale used when rendering the `render` resource (matches the doc_look default).
const RESOURCE_RENDER_SCALE: u32 = 4;

/// A parsed `atelier://` resource URI: which document, and which view of it.
#[derive(Debug, PartialEq, Eq)]
enum ResourceTarget {
    /// `atelier://doc/<id>` — the document structure JSON.
    Structure(String),
    /// `atelier://doc/<id>/render` — frame 0 PNG render (blob).
    Render(String),
}

/// Parse an `atelier://doc/<id>` or `.../render` URI into a [`ResourceTarget`].
/// Returns None for any other scheme/shape (the caller maps that to an error).
fn parse_resource_uri(uri: &str) -> Option<ResourceTarget> {
    let rest = uri.strip_prefix("atelier://doc/")?;
    match rest.strip_suffix("/render") {
        Some(id) if !id.is_empty() => Some(ResourceTarget::Render(id.to_string())),
        Some(_) => None, // "atelier://doc//render"
        None if !rest.is_empty() && !rest.contains('/') => {
            Some(ResourceTarget::Structure(rest.to_string()))
        }
        None => None, // empty id, or extra path segments
    }
}

/// Standard base64 (no line wrapping) — the blob field wants pre-encoded text and
/// we'd rather not pull in a crate for ~15 lines.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

// --- prompts ---------------------------------------------------------------

use rmcp::model::{
    GetPromptRequestParams, GetPromptResult, ListPromptsResult, Prompt, PromptArgument,
    PromptMessage, PromptMessageRole,
};

/// Looks up an argument value by name (None when absent — the builder defaults).
type ArgLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// One packaged workflow: its name, what it does, the args it takes (name +
/// whether required), and a builder that fills the template from those args.
struct PromptSpec {
    name: &'static str,
    description: &'static str,
    /// (arg name, description, required) — advertised verbatim as the schema.
    args: &'static [(&'static str, &'static str, bool)],
    /// Tool names the rendered text names verbatim; the drift test asserts each
    /// appears in the text AND in the live tool list.
    #[cfg_attr(not(test), allow(dead_code))]
    tools: &'static [&'static str],
    /// Render the prompt text from the looked-up argument values.
    build: fn(&ArgLookup) -> String,
}

/// Look up an argument from the request's object, defaulting when absent so an
/// optional arg still substitutes cleanly into the template.
fn arg<'a>(args: &'a Option<serde_json::Map<String, Value>>, key: &str) -> Option<&'a str> {
    args.as_ref()?.get(key)?.as_str()
}

/// The three shipped workflows. Each `build` emits ordered, numbered steps with
/// tool names written verbatim — the get_prompt drift test checks every name
/// here against the live tool list.
const PROMPTS: &[PromptSpec] = &[
    PromptSpec {
        name: "pixel-sprite",
        description:
            "Draw a single pixel-art sprite the right way: lock a ramp, paint, LOOK, audit.",
        args: &[
            ("subject", "What to draw, e.g. \"a knight\".", true),
            ("size", "Canvas size in pixels (default 32).", false),
            (
                "palette_hint",
                "Colour direction, e.g. \"cool steel\".",
                false,
            ),
        ],
        tools: &[
            "doc_create",
            "doc_palette",
            "doc_set_palette",
            "doc_draw",
            "doc_batch",
            "doc_look",
            "doc_fx",
            "doc_critique",
            "doc_silhouette",
            "doc_components",
            "doc_palette_report",
            "doc_export",
        ],
        build: |g| {
            let subject = g("subject").unwrap_or_else(|| "a sprite".into());
            let size = g("size").unwrap_or_else(|| "32".into());
            let palette = g("palette_hint").unwrap_or_else(|| "your choice".into());
            format!(
                "Draw a pixel-art sprite of {subject} on a {size}x{size} canvas (palette: {palette}).\n\
                 1. doc_create a {size}x{size} document.\n\
                 2. doc_palette (scheme=mono) a base colour into shades, then doc_set_palette to LOCK the ramp.\n\
                 3. Block the silhouette with doc_draw (op=rect/ellipse/polygon).\n\
                 4. Paint detail with doc_batch (many ops in one call) or doc_draw (op=pencil/line).\n\
                 5. doc_look after every burst — it returns the frame INLINE; study it before continuing.\n\
                 6. Shade with doc_fx op=shade and clean strokes with doc_fx op=pixel_perfect.\n\
                 7. Audit shape: doc_silhouette (readable bbox/fill) and doc_components (no stray specks).\n\
                 8. Audit colour: doc_palette_report (every colour in_palette, no near-dupes).\n\
                 9. doc_critique for the failure modes you can't see; fix what it flags, doc_look to confirm.\n\
                 10. doc_export op=sheet the finished sprite to a PNG.\n\
                 Iterate 3-9 until the sprite reads cleanly at 1x."
            )
        },
    },
    PromptSpec {
        name: "walk-cycle",
        description:
            "Animate a 4-pose walk cycle: reuse frames, change only what moves, verify spacing.",
        args: &[
            ("character", "Who walks, e.g. \"the knight\".", true),
            (
                "frames",
                "Total frames (default 4: contact/down/passing/up).",
                false,
            ),
        ],
        tools: &[
            "doc_frame",
            "doc_draw",
            "doc_region",
            "doc_keyframe_move",
            "doc_look",
            "doc_contact_sheet",
            "doc_frame_diff",
            "doc_anim_audit",
            "doc_add_tag",
            "doc_export",
        ],
        build: |g| {
            let character = g("character").unwrap_or_else(|| "the character".into());
            let frames = g("frames").unwrap_or_else(|| "4".into());
            format!(
                "Animate a {frames}-frame walk cycle for {character}.\n\
                 1. Start from a finished standing pose on frame 0 (use the pixel-sprite flow first).\n\
                 2. The cycle is contact -> down -> passing -> up. Plan numbers FIRST: stride ~1/3 of \
                 the character's height in px, body bobs 1px DOWN on contact and UP on passing, arms \
                 counter-swing the legs. NEVER doc_dissolve poses — it cross-fades (ghost frames), it does \
                 not move limbs.\n\
                 3. doc_frame op=add with copy_from the previous frame so each pose starts from the last.\n\
                 4. Repaint ONLY what changes per pose (legs, arms) with doc_draw (op=pencil) / doc_region op=move; \
                 doc_keyframe_move eases a region across several frames in one call.\n\
                 5. doc_look every frame (onion=true ghosts the neighbours); doc_contact_sheet shows \
                 the whole cycle in one inline grid.\n\
                 6. Verify each adjacent pair with doc_frame_diff (only the limbs should change).\n\
                 7. doc_anim_audit mode=\"spacing\" — the per-frame motion must be even, low drift.\n\
                 8. doc_add_tag the range and doc_anim_audit mode=\"seam\" so the loop wrap is clean.\n\
                 9. doc_frame op=duration ~120ms per frame, with contact poses held ~1.5x longer — \
                 uniform 100ms reads mechanical.\n\
                 10. doc_export op=anim the tagged loop and study it.\n\
                 Iterate 4-9 until the walk reads smoothly."
            )
        },
    },
    PromptSpec {
        name: "seamless-tile",
        description: "Paint a seamless tile: wrap edges, prove the seam is zero, eyeball the grid.",
        args: &[
            ("material", "What to tile, e.g. \"grass\".", true),
            ("size", "Tile size in pixels (default 32).", false),
        ],
        tools: &[
            "doc_create",
            "doc_set_palette",
            "doc_draw",
            "doc_look",
            "doc_fx",
            "doc_seam_report",
            "doc_palette_report",
            "doc_export",
        ],
        build: |g| {
            let material = g("material").unwrap_or_else(|| "the material".into());
            let size = g("size").unwrap_or_else(|| "32".into());
            format!(
                "Paint a seamless {size}x{size} {material} tile.\n\
                 1. doc_create a {size}x{size} document and doc_set_palette to lock the colours.\n\
                 2. Fill the base with doc_draw op=fill_cel, then texture with doc_draw op=noise / op=scatter.\n\
                 3. doc_look to study the raw tile inline.\n\
                 4. doc_fx op=shift wrap=true to roll the seam into the middle, then paint over the join.\n\
                 5. Use doc_fx op=shift wrap=true again to make detail variants without breaking edges.\n\
                 6. doc_seam_report MUST return zero mismatches on both axes — fix until it does.\n\
                 7. doc_look tile=2 and eyeball the 2x2 grid for any visible repeat or seam.\n\
                 8. doc_palette_report to confirm the texture stayed on-palette.\n\
                 9. Repeat 4-7 until the seam report is clean and the grid looks continuous.\n\
                 10. doc_export op=sheet the tile to a PNG.\n\
                 The tile is done only when doc_seam_report is zero."
            )
        },
    },
    PromptSpec {
        name: "game-asset-set",
        description:
            "Build a coherent game's asset set — hero moveset, terrain, HUD — audited as ONE work.",
        args: &[
            (
                "theme",
                "The game's look, e.g. \"forest ruins, dusk\".",
                true,
            ),
            (
                "hero",
                "The playable character, e.g. \"a cloaked ranger\".",
                true,
            ),
            (
                "size",
                "Character canvas size in pixels (default 48).",
                false,
            ),
        ],
        tools: &[
            "doc_create",
            "doc_palette",
            "doc_set_palette",
            "doc_figure",
            "doc_pose_cycle",
            "doc_walk",
            "doc_look",
            "doc_autotile_set",
            "doc_tilemap_assemble",
            "doc_panel",
            "doc_nine_slice",
            "doc_set_audit",
            "doc_set_palette_sync",
            "doc_colorblind_check",
            "doc_export",
            "export_atlas",
        ],
        build: |g| {
            let theme = g("theme").unwrap_or_else(|| "the game's theme".into());
            let hero = g("hero").unwrap_or_else(|| "the hero".into());
            let size = g("size").unwrap_or_else(|| "48".into());
            format!(
                "Build a coherent asset SET for a game: theme {theme}, hero {hero}. \
                 Name every document with one family prefix (e.g. game-hero-idle, game-tile-grass) so the set tools can find them.\n\
                 1. ONE palette first: doc_palette a scheme fitting {theme}; you will lock this exact ramp on every document.\n\
                 2. Hero: doc_create a {size}x{size} doc per animation; doc_set_palette; pose {hero} once as 13 joints (doc_figure to preview), then doc_walk for the walk and doc_pose_cycle for idle, run, jump, attack, hurt — the same joints every time.\n\
                 3. doc_look each cycle inline; re-run a gait with different intensity/frames until the motion reads.\n\
                 4. Terrain: doc_create a tile-sized doc, layer 0 inner material, layer 1 outer; doc_autotile_set for the 47-tile family, then doc_tilemap_assemble a test mask and doc_look the MAP — terrain is judged assembled, never as lone tiles.\n\
                 5. HUD: doc_create a UI doc; author ONE panel (doc_panel), then doc_nine_slice it to every dialog/button size.\n\
                 6. Cohesion gate: doc_set_audit on the family prefix — fix every warning; doc_set_palette_sync from the hero doc if palettes drifted.\n\
                 7. doc_colorblind_check the HUD and any state-colour art; recolour pairs that collapse.\n\
                 8. Ship: doc_export op=anim per gait tag, doc_export op=tileset for terrain, export_atlas for the whole set.\n\
                 The set is done only when doc_set_audit says cohesive."
            )
        },
    },
];

/// Build the advertised [`Prompt`] descriptors from [`PROMPTS`].
fn prompt_specs() -> Vec<Prompt> {
    PROMPTS
        .iter()
        .map(|p| {
            let args = p
                .args
                .iter()
                .map(|(name, desc, required)| {
                    PromptArgument::new(*name)
                        .with_description(*desc)
                        .with_required(*required)
                })
                .collect();
            Prompt::new(p.name, Some(p.description), Some(args))
        })
        .collect()
}

/// Render one prompt's filled text + description from request arguments, or None
/// if the name is unknown (the caller maps that to an error).
fn build_prompt(
    name: &str,
    args: &Option<serde_json::Map<String, Value>>,
) -> Option<(String, String)> {
    let spec = PROMPTS.iter().find(|p| p.name == name)?;
    let get = |k: &str| arg(args, k).map(str::to_string);
    Some(((spec.build)(&get), spec.description.to_string()))
}

// --- server ----------------------------------------------------------------

#[derive(Clone)]
pub struct Atelier {
    /// Shared so concurrent HTTP sessions serialise document file writes.
    studio: std::sync::Arc<std::sync::Mutex<Studio>>,
    tool_router: ToolRouter<Self>,
    /// Optional session recorder; when set, each tool call is logged to a recipe.
    recorder: Option<Recorder>,
    /// Optional doc-change notifier (HTTP only): after a successful tool call its
    /// `doc_id` is broadcast so the live `/gallery/events` SSE stream can push a
    /// "this doc changed" event and the browser re-renders without polling.
    change_tx: Option<tokio::sync::broadcast::Sender<String>>,
}

impl Atelier {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::with_studio(std::sync::Arc::new(std::sync::Mutex::new(Studio::new())))
    }

    pub fn with_studio(studio: std::sync::Arc<std::sync::Mutex<Studio>>) -> Self {
        Self {
            studio,
            tool_router: Self::tool_router(),
            recorder: None,
            change_tx: None,
        }
    }

    /// Enable session recording: every tool call is appended to a recipe at `path`.
    fn with_recording(mut self, path: std::path::PathBuf) -> Self {
        self.recorder = Some(Recorder::new(path));
        self
    }

    fn studio(&self) -> std::sync::MutexGuard<'_, Studio> {
        self.studio.lock().expect("studio lock poisoned")
    }

    /// Enumerate every browsable resource: per document, its structure JSON and a
    /// frame-0 PNG render, each with a human-readable name.
    fn list_resource_specs(&self) -> Vec<Resource> {
        let docs = self.studio().list_docs();
        let mut out = Vec::new();
        for d in docs["documents"].as_array().into_iter().flatten() {
            let Some(id) = d["id"].as_str() else { continue };
            let name = d["name"].as_str().unwrap_or(id);
            out.push(
                RawResource::new(format!("atelier://doc/{id}"), format!("{name} (structure)"))
                    .with_description("Document structure: layers, frames, cels, tags.")
                    .with_mime_type("application/json")
                    .no_annotation(),
            );
            out.push(
                RawResource::new(
                    format!("atelier://doc/{id}/render"),
                    format!("{name} (render)"),
                )
                .with_description("Frame 0 flattened to a PNG at scale 4.")
                .with_mime_type("image/png")
                .no_annotation(),
            );
        }
        out
    }

    /// Resolve a parsed [`ResourceTarget`] to its contents (structure JSON text or
    /// a PNG blob). Studio errors surface as `resource_not_found`.
    fn fetch_resource(
        &self,
        uri: &str,
        target: ResourceTarget,
    ) -> Result<ResourceContents, String> {
        match target {
            ResourceTarget::Structure(id) => {
                let v = self.studio().doc_info(&id)?;
                Ok(ResourceContents::text(j(v), uri).with_mime_type("application/json"))
            }
            ResourceTarget::Render(id) => {
                let png = self
                    .studio()
                    .render_png_bytes(&id, 0, RESOURCE_RENDER_SCALE)?;
                Ok(ResourceContents::blob(base64(&png), uri).with_mime_type("image/png"))
            }
        }
    }
}

#[tool_router(router = tool_router)]
impl Atelier {
    // -- library --
    #[tool(
        description = "Create an editable document (layered canvas + timeline). Returns its id + structure."
    )]
    async fn doc_create(&self, Parameters(p): Parameters<DocCreate>) -> CallToolResult {
        res(self.studio().doc_create(&p.name, p.width, p.height))
    }

    #[tool(
        description = "List documents (id, name, size, frame/layer counts). Optional `prefix` selects a family by id start (`hero-` matches hero-idle, hero-run); `contains` filters by substring; both = AND. Omit both to list everything."
    )]
    async fn list_docs(&self, Parameters(p): Parameters<ListDocs>) -> CallToolResult {
        res(Ok(self.studio().list_docs_filtered(
            p.prefix.as_deref(),
            p.contains.as_deref(),
        )))
    }

    #[tool(description = "Get a document's structure: layers, frames, cels, tags.")]
    async fn doc_info(&self, Parameters(p): Parameters<DocRef>) -> CallToolResult {
        res(self.studio().doc_info(&p.doc_id))
    }

    #[tool(
        description = "Audit N documents as ONE game — the set-level doc_critique. Resolve members by `ids` and/or id `prefix` (union), then report per-doc palette/value/scale/pivot stats plus set cohesion: palette union size + unlocked docs + cross-doc near-duplicate colours (OKLab ΔE), silhouette-height scale outliers vs the set median, the set value range, and docs missing pivots. Verdict is 'cohesive' or a list of actionable warnings (e.g. run doc_set_palette_sync). This is how a pile of sprites becomes one game."
    )]
    async fn doc_set_audit(&self, Parameters(p): Parameters<DocSetAudit>) -> CallToolResult {
        res(self
            .studio()
            .doc_set_audit(p.ids.as_deref(), p.prefix.as_deref()))
    }

    #[tool(
        description = "Broadcast ONE palette across a document set: lock it on every member (`ids` and/or id `prefix`) and perceptually snap every cel onto it (OKLab nearest, alpha preserved). Palette = explicit `palette` colours or `from_doc` to copy another document's locked palette. Returns per-doc moved-pixel counts. The fix for doc_set_audit palette warnings."
    )]
    async fn doc_set_palette_sync(
        &self,
        Parameters(p): Parameters<DocSetPaletteSync>,
    ) -> CallToolResult {
        let palette = p
            .palette
            .as_ref()
            .map(|v| v.iter().map(|c| rgba(c)).collect::<Vec<[u8; 4]>>());
        res(self.studio().doc_set_palette_sync(
            p.ids.as_deref(),
            p.prefix.as_deref(),
            palette,
            p.from_doc.as_deref(),
        ))
    }

    #[tool(description = "Delete a document and all its files.")]
    async fn delete_doc(&self, Parameters(p): Parameters<DocRef>) -> CallToolResult {
        res(self.studio().delete_doc(&p.doc_id))
    }

    #[tool(
        description = "Export every document as a spritesheet PNG (+ JSON meta) into a flat target dir."
    )]
    async fn export_all(&self, Parameters(p): Parameters<ExportAll>) -> CallToolResult {
        res(self
            .studio()
            .export_all(&p.target_dir, p.scale.unwrap_or(4)))
    }

    #[tool(
        description = "Pack EVERY frame of EVERY document into one atlas PNG + master JSON map (doc/frame/rect/duration/pivot) for slicing a whole game's sprites from a single texture. max_width wraps the shelf packer."
    )]
    async fn export_atlas(&self, Parameters(p): Parameters<ExportAtlas>) -> CallToolResult {
        res(self.studio().export_atlas(
            &p.out_path,
            p.scale.unwrap_or(4),
            p.max_width.unwrap_or(512),
        ))
    }

    // -- documents: editable layered/timeline sprites --
    #[tool(
        description = "Layer structure in one tool. `op`: add (new layer on top — name/opacity/blend) · set (change layer `index`'s visible/opacity/blend; omit a field to leave it) · move (`index`→`to_index`) · insert (new layer at `index`) · delete · rename · duplicate · merge_down (`index` onto the layer below). Blend ∈ normal/multiply/screen/add/overlay/soft-light/hard-light/darken/lighten/color-dodge/color-burn/difference/subtract/exclusion."
    )]
    async fn doc_layer(&self, Parameters(p): Parameters<DocLayer>) -> CallToolResult {
        res(self.studio().doc_layer(
            &p.doc_id, &p.op, p.index, p.to_index, p.name, p.visible, p.opacity, p.blend,
        ))
    }

    #[tool(
        description = "Frame lifecycle + timing in one tool. `op`: add (append a frame; `copy_from` duplicates that frame's cels, `duration_ms` default 100) · duration (set frame `frame`'s `duration_ms`) · insert (new frame at `frame`) · duplicate (`frame`) · delete (`frame`; last frame protected) · move (`frame`→`to_index`). Cels reindex and tag ranges remap. (Pivots, collision boxes, tags and keyframe motion have their own tools.)"
    )]
    async fn doc_frame(&self, Parameters(p): Parameters<DocFrame>) -> CallToolResult {
        let studio = self.studio();
        // delete destroys cels and move can scramble tags — auto-checkpoint the
        // destructive ops, mirroring the old doc_frame_ops behaviour.
        if p.op == "delete" || p.op == "move" {
            studio.auto_checkpoint(&p.doc_id, "doc_frame");
        }
        res(studio.doc_frame(
            &p.doc_id,
            &p.op,
            p.frame,
            p.copy_from,
            p.to_index,
            p.duration_ms,
        ))
    }

    #[tool(
        description = "Add an animation tag (named frame range). direction: forward/reverse/pingpong."
    )]
    async fn doc_add_tag(&self, Parameters(p): Parameters<DocAddTag>) -> CallToolResult {
        res(self.studio().doc_add_tag(
            &p.doc_id,
            &p.name,
            p.from,
            p.to,
            p.direction.as_deref().unwrap_or("forward"),
        ))
    }

    #[tool(description = "Clear (empty) a layer×frame cel.")]
    async fn doc_clear_cel(&self, Parameters(p): Parameters<DocCel>) -> CallToolResult {
        res(self.studio().doc_clear_cel(&p.doc_id, p.layer, p.frame))
    }

    #[tool(
        description = "Region + clipboard ops on a cel. `op`: copy (rect [x0,y0,x1,y1] → clipboard) · cut (copy + clear) · clear (erase the rect) · move (shift the rect by dx,dy in place) · paste (clipboard at x,y; `blend` source-over by default, false overwrites). Clipboard is cross-document. (External-image import is doc_stamp_image; lifting a part onto its own layer is doc_extract_to_layer.)"
    )]
    async fn doc_region(&self, Parameters(p): Parameters<DocRegion>) -> CallToolResult {
        res(self.studio().doc_region(
            &p.doc_id, &p.op, p.layer, p.frame, p.x0, p.y0, p.x1, p.y1, p.dx, p.dy, p.x, p.y,
            p.blend,
        ))
    }

    #[tool(
        description = "Place an external PNG into a cel at (x,y) — import bridge for AI-gen/real/Figma art. Optional `scale` (nearest-neighbour) and `rotate` (degrees). By default draws OVER existing content with `opacity`+`blend` (sub-sprite reuse, no layer-per-element); `replace`=true overwrites the whole cel. Honours an active selection. Returns an INLINE preview of the stamped cel."
    )]
    async fn doc_stamp_image(&self, Parameters(p): Parameters<DocStampImage>) -> CallToolResult {
        let studio = self.studio();
        let r = studio.doc_stamp_image(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x.unwrap_or(0),
            p.y.unwrap_or(0),
            &p.png_path,
            p.scale.unwrap_or(1.0),
            p.target_w,
            p.rotate.unwrap_or(0.0),
            p.opacity.unwrap_or(255),
            p.blend.as_deref().unwrap_or("normal"),
            p.replace.unwrap_or(false),
        );
        previewed(&studio, &p.doc_id, Some(p.layer), p.frame, r)
    }

    #[tool(
        description = "Generate the deterministic 16-tile Wang/blob terrain set from a source doc: frame 0's layer 0 holds the INNER material, layer 1 the OUTER (top-left N×N of each is sampled). Creates a NEW document <id>-wang (canvas 4N×4N) holding all 16 corner combinations in a 4×4 grid (tile index = NE,SE,SW,NW corner bits); each set bit fills a quarter-disc (radius N/2) at that corner, adjacent set corners connect along their shared edge. Returns the new doc's structure + id."
    )]
    async fn doc_wang_tiles(&self, Parameters(p): Parameters<DocWangTiles>) -> CallToolResult {
        res(self.studio().wang_tiles(&p.doc_id, p.n))
    }

    #[tool(
        description = "TRUE 9-slice: author a panel ONCE (any style — bevels, rounded corners, ornate borders), then emit it at ANY size. The `src` rect is cut into a 3×3 grid by `inset`: corners copy verbatim, edges and centre tile (default) or stretch to fill `dst`. Transparent source pixels are skipped, so rounded panels keep their shape. The dialog-box / button / HUD-frame workhorse: draw one 12×12 panel, stamp every UI size from it."
    )]
    async fn doc_nine_slice(&self, Parameters(p): Parameters<DocNineSlice>) -> CallToolResult {
        let rect = |v: &Vec<i64>| -> Result<(i32, i32, i32, i32), String> {
            if v.len() != 4 {
                return Err("rect must be [x, y, w, h]".into());
            }
            Ok((v[0] as i32, v[1] as i32, v[2] as i32, v[3] as i32))
        };
        let (src, dst) = match (rect(&p.src), rect(&p.dst)) {
            (Ok(s), Ok(d)) => (s, d),
            (Err(e), _) | (_, Err(e)) => return res(Err(e)),
        };
        res(self.studio().nine_slice(
            &p.doc_id,
            p.layer,
            p.frame,
            p.src_layer.unwrap_or(p.layer),
            p.src_frame.unwrap_or(0),
            src,
            p.inset.unwrap_or(3) as i32,
            dst,
            p.mode.as_deref().unwrap_or("tile"),
        ))
    }

    #[tool(
        description = "Seeded PARTICLE EMITTER rendered to frames — sparks, embers, smoke, rain, magic motes. Particles spawn inside `region`, fly along `angle ± spread` at `speed` under `gravity`, fade + shrink over `life`, coloured birth→death along the ramp (auto-ramped from `color` if omitted). Fully deterministic in `seed` and phase-staggered so the animation LOOPS cleanly. Draws onto `layer` across `frames` (clearing each cel) and tags the range `emit` — put it on its own layer over the art. Export with doc_export op=anim tag=emit."
    )]
    async fn doc_emit(&self, Parameters(p): Parameters<DocEmit>) -> CallToolResult {
        if p.region.len() != 4 {
            return res(Err("region must be [x, y, w, h]".into()));
        }
        let ramp = p
            .ramp
            .as_ref()
            .map(|v| v.iter().map(|c| rgba(c)).collect::<Vec<[u8; 4]>>());
        res(self.studio().emit(
            &p.doc_id,
            p.layer,
            (
                p.region[0] as i32,
                p.region[1] as i32,
                p.region[2] as i32,
                p.region[3] as i32,
            ),
            p.frames.unwrap_or(8),
            p.count.unwrap_or(24),
            p.angle.unwrap_or(270.0),
            p.spread.unwrap_or(30.0),
            p.speed.unwrap_or(1.5),
            p.gravity.unwrap_or(0.0),
            p.life.unwrap_or(1.0),
            p.size.unwrap_or(2) as i32,
            p.seed.unwrap_or(1),
            rgba(&p.color),
            ramp,
        ))
    }

    #[tool(
        description = "Colour-vision-deficiency audit: simulate protanopia / deuteranopia / tritanopia over the flattened frame and report which of the art's distinct colour pairs — readable to typical vision — COLLAPSE under each simulation (OKLab ΔE falls below the readable floor). Returns an INLINE side-by-side strip (normal · protan · deutan · tritan) plus the collapsing pairs, so 'is my health bar readable to 8% of players?' is one call. Run before shipping UI/state colours."
    )]
    async fn doc_colorblind_check(
        &self,
        Parameters(p): Parameters<DocColorblindCheck>,
    ) -> CallToolResult {
        img_result(self.studio().doc_colorblind_check(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.scale.unwrap_or(2),
        ))
    }

    #[tool(
        description = "Generate the deterministic 47-tile BLOB autotile set — the full edge+corner bitmask family (the modern superset of the 16-corner Wang set). Same source contract as doc_wang_tiles: frame 0, layer 0 = INNER material, layer 1 = OUTER, top-left N×N sampled. Creates a NEW document <id>-blob (7N×7N, the 47 canonical neighbour masks in a 7×7 grid) and returns `masks` — the 8-bit neighbour mask per grid index (N=1 NE=2 E=4 SE=8 S=16 SW=32 W=64 NW=128) — so an engine autotiler maps straight onto the sheet. Export with doc_export op=tileset. See it in situ FIRST with doc_tilemap_assemble."
    )]
    async fn doc_autotile_set(&self, Parameters(p): Parameters<DocAutotileSet>) -> CallToolResult {
        res(self.studio().autotile_set(&p.doc_id, p.n))
    }

    #[tool(
        description = "Assemble a TILEMAP from a terrain mask — the in-situ test of an autotile set, and the only real one. `rows` = the map as strings (`#`/`1`/`x` = filled); every filled cell computes its 8-neighbour mask and renders straight from the source materials (layer 0 inner / layer 1 outer) with the same blob rules as doc_autotile_set, so what you see IS what the tile family produces in a real map. `outside` = filled (default: terrain continues past the map edge) | empty (borders get outlines). Creates a NEW document <id>-map — doc_look it to judge the terrain reads, then export."
    )]
    async fn doc_tilemap_assemble(
        &self,
        Parameters(p): Parameters<DocTilemapAssemble>,
    ) -> CallToolResult {
        let outside_filled = p.outside.as_deref().unwrap_or("filled") == "filled";
        res(self
            .studio()
            .tilemap_assemble(&p.doc_id, p.n, &p.rows, outside_filled))
    }

    #[tool(
        description = "Export a document to a file. `op`: sheet (horizontal spritesheet PNG + JSON meta — rects/durations/tags/pivots/boxes/palette; `meta`=standard writes the industry-standard hash sprite-JSON engines' existing importers parse instead — no pivots/boxes in that shape) · anim (animated `format`=gif|apng, optional `tag` plays that animation in its direction) · tileset (slice a `tile_w`×`tile_h` grid → PNG + Tiled .tsx + JSON; canvas must divide evenly). Shared: out_path, scale (sheet/anim 4, tileset 1). For a whole-library dump use export_all/export_atlas; for the Wang set use doc_wang_tiles."
    )]
    async fn doc_export(&self, Parameters(p): Parameters<DocExport>) -> CallToolResult {
        res(self
            .studio()
            .doc_export(&p.doc_id, &p.op, &p.out_path, p.scale, &p.params))
    }

    // -- per-pixel drawing on a cel (the editor; coords = document pixels) --
    #[tool(
        description = "Insert `steps` cross-faded DISSOLVE frames after frame `from`: every layer's pixels alpha-blend toward frame `to` (snapped to the locked palette), so in-betweens are semi-transparent double-exposures. ONLY for fades, FX dissolves, and impact flashes — NEVER pose/limb motion (limbs ghost instead of moving; use doc_keyframe_move or per-frame edits for that). Auto-checkpoints first; undo a bad tween with doc_checkpoint restore or doc_frame op=delete. Reindexes later cels and remaps tags."
    )]
    async fn doc_dissolve(&self, Parameters(p): Parameters<DocTween>) -> CallToolResult {
        let studio = self.studio();
        studio.auto_checkpoint(&p.doc_id, "dissolve");
        res(studio.doc_tween(
            &p.doc_id,
            p.from,
            p.to,
            p.steps.unwrap_or(1),
            p.duration_ms.unwrap_or(100),
        ))
    }

    #[tool(
        description = "Bloom/glow: blur a bright copy of the cel and composite it back through a light blend (`mode` screen/add) at `intensity`. `color` tints the glow (omit = the art's own colours). Honours an active selection."
    )]
    async fn doc_glow(&self, Parameters(p): Parameters<DocGlow>) -> CallToolResult {
        let color = p.color.as_ref().map(|c| rgba(c));
        // Default to snapping the bloom on-palette; `snap=false` keeps it soft.
        let snap = if p.snap != Some(false) {
            Some(atelier_core::document::AlphaSnap::Opaque(
                p.snap_cutoff.unwrap_or(64),
            ))
        } else {
            None
        };
        res(self.studio().doc_glow(
            &p.doc_id,
            p.layer,
            p.frame,
            color,
            p.radius.unwrap_or(2),
            p.intensity.unwrap_or(180),
            p.mode.as_deref().unwrap_or("screen"),
            snap,
        ))
    }

    #[tool(
        description = "Paint a RIM/edge light along the silhouette edges that FACE the light (`az`: 0=right, 90=down, 180=left, 270=up) — the edge-relative highlight that was otherwise hand-placed pixel by pixel. Estimates each edge pixel's outward normal and stamps `color` where it faces the light, `width` px thick, `falloff` tightens it. `dark=true` lights the away-facing edge instead (core/contact shadow). Topological — survives small canvases where a Fresnel term washes out. Honours an active selection."
    )]
    async fn doc_rim_light(&self, Parameters(p): Parameters<DocRimLight>) -> CallToolResult {
        res(self.studio().doc_rim_light(
            &p.doc_id,
            p.layer,
            p.frame,
            rgba(&p.color),
            p.az,
            p.width.unwrap_or(1),
            p.falloff.unwrap_or(1.5),
            p.dark.unwrap_or(false),
            p.snap.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Cast a projected GROUND shadow from a caster silhouette — not a flat offset copy (that's doc_drop_shadow) but the caster flattened onto its contact row and sheared AWAY from the light, so a tall shape throws a long foreshortened shadow anchored at its feet. `az` = light azimuth (0=right, 90=down, 180=left, 270=up; pairs with doc_form_audit); `length` stretches it along the ground, `squash` 0..1 is how much height survives (0 = flat). With `receiver_layer` the shadow is painted onto that layer and clipped to its opaque pixels (lands on the ground only); omit to draw behind the caster on its own cel. `snap` keeps it on-palette."
    )]
    async fn doc_cast_shadow(&self, Parameters(p): Parameters<DocCastShadow>) -> CallToolResult {
        res(self.studio().doc_cast_shadow(
            &p.doc_id,
            p.layer,
            p.frame,
            p.az.unwrap_or(135.0),
            p.length.unwrap_or(1.0),
            p.squash.unwrap_or(0.2),
            rgba(&p.color),
            p.opacity.unwrap_or(140),
            p.receiver_layer,
            p.snap.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Set/modify the active pixel selection so subsequent painting ops (fill/gradient/scatter/rect/ellipse/polygon/pencil/line/batch) are confined to it. shape: rect (x0,y0,x1,y1) | ellipse (cx,cy,rx,ry) | color (layer,frame + `color` or sample x,y + tolerance) | all | none (clear). mode: replace (default) | add | subtract | intersect."
    )]
    async fn doc_select(&self, Parameters(p): Parameters<DocSelect>) -> CallToolResult {
        let shape = p.shape.as_deref().unwrap_or("rect");
        let mode = p.mode.as_deref().unwrap_or("replace");
        let rect = match (p.x0, p.y0, p.x1, p.y1) {
            (Some(x0), Some(y0), Some(x1), Some(y1)) => Some((x0, y0, x1, y1)),
            _ => None,
        };
        let ell = match (p.cx, p.cy, p.rx, p.ry) {
            (Some(cx), Some(cy), Some(rx), Some(ry)) => Some((cx, cy, rx, ry)),
            _ => None,
        };
        let color_at = if shape == "color" {
            Some(atelier_studio::ColorSelect {
                layer: p.layer.unwrap_or(0),
                frame: p.frame.unwrap_or(0),
                color: p.color.as_ref().map(|c| rgba(c)),
                sample: match (p.x, p.y) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => None,
                },
                tol: p.tolerance.unwrap_or(0),
            })
        } else {
            None
        };
        res(self
            .studio()
            .doc_select(&p.doc_id, shape, mode, rect, ell, color_at))
    }

    // -- selection / region / clipboard (move limbs, reuse art) --
    // -- canvas readers (read-only analysis to SEE the canvas as data) --
    #[tool(
        description = "Dump a region of a frame as a text grid so you can read exact pixels blind. mode=\"symbol\" maps each distinct colour to a glyph (A..Z a..z 0..9) with a legend, `.`=transparent; mode=\"hex\" emits #rrggbb(aa)/`.` tokens. `layer` dumps one cel (omit = flattened). `region` [x0,y0,x1,y1] caps at 4096 px — crop large canvases."
    )]
    async fn doc_dump_region(&self, Parameters(p): Parameters<DocDumpRegion>) -> CallToolResult {
        res(self.studio().doc_dump_region(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            region(&p.region),
            p.mode.as_deref().unwrap_or("symbol"),
        ))
    }

    #[tool(
        description = "Opaque-vs-transparent shape report for a frame: tight bbox, fill_ratio (opaque/canvas), and a #/. grid of the whole canvas. `layer` reads one cel (omit = flattened). `alpha_threshold` is the min alpha counted opaque (default 1). Read a sprite's silhouette/readability at a glance."
    )]
    async fn doc_silhouette(&self, Parameters(p): Parameters<DocSilhouette>) -> CallToolResult {
        res(self.studio().doc_silhouette(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            p.alpha_threshold.unwrap_or(1),
        ))
    }

    #[tool(
        description = "Connected-component analysis of a frame: each blob's bbox, centroid, area and dominant colour (sorted by area desc, capped 64). `connectivity` 4|8 (default 8); `color` restricts to that exact colour (omit = any opaque); `min_area` filters the list. Stray 1–2px `specks` are always reported — catches orphan/leftover pixels."
    )]
    async fn doc_components(&self, Parameters(p): Parameters<DocComponents>) -> CallToolResult {
        let color = p.color.as_ref().filter(|c| c.len() >= 3).map(|c| rgba(c));
        res(self.studio().doc_components(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            p.connectivity.unwrap_or(8),
            color,
            p.min_area.unwrap_or(1),
        ))
    }

    #[tool(
        description = "Per-form shading audit — sees the #1 beginner failure the scalar reports can't. For each connected opaque form it infers the light direction (least-squares fit of perceptual lightness → `light_azimuth_deg`, `plane_fit_r2`) and flags PILLOW-SHADING (`pillow_corr`: brightness hugging the silhouette centre instead of a light direction). The summary reports `pillow_forms`, the shared `dominant_light_azimuth_deg` / `light_spread_deg`, and a verdict (ok | pillow-shading detected | inconsistent light direction). `min_area` skips specks (default 12). Deterministic; run before relight/form or before export."
    )]
    async fn doc_form_audit(&self, Parameters(p): Parameters<DocFormAudit>) -> CallToolResult {
        res(self.studio().doc_form_audit(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            p.min_area.unwrap_or(12),
        ))
    }

    #[tool(
        description = "Coarse coverage/composition heatmap: split the flattened frame into rows×cols cells (default 8×8), each reporting opaque fill 0..1 and mean luma (null if empty), plus the content bbox and its centre offset from the canvas centre. Check balance/placement/negative space."
    )]
    async fn doc_coverage_map(&self, Parameters(p): Parameters<DocCoverageMap>) -> CallToolResult {
        res(self.studio().doc_coverage_map(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.cols.unwrap_or(8),
            p.rows.unwrap_or(8),
        ))
    }

    // -- value & colour feedback (read-only analysis to judge values/colour) --
    #[tool(
        description = "Contrast as a number (the WCAG luminance-ratio formula). NOTE: WCAG is a text-on-UI legibility standard, not a sprite-vs-scene readability metric — treat the ratio as a value-separation HINT, not a pass/fail. mode=\"region\" compares the mean colour inside `region` [x0,y0,x1,y1] against its 4px surround → {ratio}. mode=\"palette\" scores every pair of the frame's distinct opaque colours (capped 16 — quantize first if more) and lists the lowest-contrast pairs. mode=\"one-bit\" thresholds luma to a pure B/W PNG (returns path + black/white %) — the real silhouette-readability check. `min_ratio` (default 1.5) only flags pairs below it."
    )]
    async fn doc_contrast_check(
        &self,
        Parameters(p): Parameters<DocContrastCheck>,
    ) -> CallToolResult {
        res(self.studio().doc_contrast_check(
            &p.doc_id,
            p.frame.unwrap_or(0),
            &p.mode,
            region(&p.region),
            p.min_ratio.unwrap_or(1.5),
            p.threshold.unwrap_or(128),
            p.out_path.as_deref(),
        ))
    }

    #[tool(
        description = "Colour-usage report for a frame (or all frames): every distinct opaque colour with pixel count, percent, and in_palette (null when the doc has no locked palette), sorted by usage. Flags off-palette colours and near-duplicate pairs (max channel distance ≤ dupe_threshold, default 8). `layer` reads one cel; `region` restricts the tally. Audit colour discipline / find stray shades."
    )]
    async fn doc_palette_report(
        &self,
        Parameters(p): Parameters<DocPaletteReport>,
    ) -> CallToolResult {
        res(self.studio().doc_palette_report(
            &p.doc_id,
            p.frame,
            p.layer,
            region(&p.region),
            p.dupe_threshold.unwrap_or(8),
        ))
    }

    #[tool(
        description = "Validate a colour ramp's craft from explicit `colors` [[r,g,b],...] (≥2) OR a `doc_id`'s locked palette (optional [start,end) `slice`). Returns monotonic_value, value_deltas, even_spacing (max step deviation ≤25% of mean), per-step hue_shift_deg (signed shortest-arc), hue_direction (warm-to-cool|cool-to-warm|mixed|none), sat_arc, and warnings (e.g. value reversals). Doc-independent."
    )]
    async fn doc_ramp_validate(
        &self,
        Parameters(p): Parameters<DocRampValidate>,
    ) -> CallToolResult {
        let colors = p
            .colors
            .as_ref()
            .map(|cs| cs.iter().map(|c| rgba(c)).collect::<Vec<_>>());
        let slice = p
            .slice
            .as_ref()
            .filter(|s| s.len() >= 2)
            .map(|s| (s[0], s[1]));
        res(self
            .studio()
            .doc_ramp_validate(colors, p.doc_id.as_deref(), slice))
    }

    // -- animation & tiling feedback (read-only) + keyframe write --
    #[tool(
        description = "Diff two frames pixel-by-pixel: returns changed/added/removed/recolored counts and the change_bbox. `layer` diffs one cel (omit = flattened). `region` [x0,y0,x1,y1] restricts the area. grid=true adds a text map (`.`unchanged `+`added `-`removed `~`recolored, area capped 4096 px). render=\"overlay\" returns an INLINE PNG of frame_b dimmed 40% with changed pixels flagged (green=added/red=removed/yellow=recoloured). Inspect what actually moved between animation frames."
    )]
    async fn doc_frame_diff(&self, Parameters(p): Parameters<DocFrameDiff>) -> CallToolResult {
        opt_img_result(self.studio().doc_frame_diff(
            &p.doc_id,
            p.frame_a,
            p.frame_b,
            p.layer,
            region(&p.region),
            p.grid.unwrap_or(false),
            p.render.as_deref().unwrap_or("none"),
            p.out_path.as_deref(),
            p.scale.unwrap_or(4),
        ))
    }

    #[tool(
        description = "Tiling seam check: wrap-test a frame's far edge against the near edge it abuts when repeated. axis=\"horizontal\" tests left↔right, \"vertical\" top↔bottom, \"both\" runs each. Per axis returns {mismatches, max_delta, worst:[[x,y,delta] ≤10]}; any mismatch also returns an INLINE overlay PNG (frame dimmed, bad edge pixels red) so you see WHERE the seam pops. `threshold` is the max per-channel delta still counted a match (default 0). Verify seamless tiles."
    )]
    async fn doc_seam_report(&self, Parameters(p): Parameters<DocSeamReport>) -> CallToolResult {
        opt_img_result(self.studio().doc_seam_report(
            &p.doc_id,
            p.layer,
            p.frame.unwrap_or(0),
            p.axis.as_deref().unwrap_or("both"),
            p.threshold.unwrap_or(0),
            p.out_path.as_deref(),
        ))
    }

    #[tool(
        description = "Audit an animation loop. mode=\"seam\" diffs the wrap the loop actually plays and returns seam_score = changed/opaque plus the change_bbox naming WHERE the loop pops. mode=\"spacing\" tracks the opaque-mass CENTROID per played frame (per_frame_center/offset, total_drift, evenness; 0 = mechanically even); pass `region` to isolate one part (a swinging arm) over a static body. mode=\"arc\" returns the centroid trajectory, arc_residual (~0 = mechanical straight slide; higher = proper arc) and volume_cv (~0 = constant mass). mode=\"timing\" returns per-frame durations and flags uniform timing (reads mechanical — hold contacts ~1.5x). `tag` audits one tag (omit = whole timeline)."
    )]
    async fn doc_anim_audit(&self, Parameters(p): Parameters<DocAnimAudit>) -> CallToolResult {
        res(self.studio().doc_anim_audit(
            &p.doc_id,
            p.tag.as_deref(),
            p.layer,
            &p.mode,
            region(&p.region),
        ))
    }

    #[tool(
        description = "Eased multi-frame region motion. Reads the `region` [x0,y0,x1,y1] content from `from_frame` and stamps it (source-over) into every frame in (from_frame, to_frame] at an eased fraction of the total (dx,dy); to_frame gets the full offset. easing: linear/ease-in/ease-out/ease-in-out (cubic), bounce, overshoot (shoots past then settles), elastic (decaying oscillation). clear_source=true (default) clears the original rect in each destination frame so a moving limb leaves no stale copy. Frames must already exist (else error — doc_frame op=add first). Returns frames_touched + per-frame offsets."
    )]
    async fn doc_keyframe_move(
        &self,
        Parameters(p): Parameters<DocKeyframeMove>,
    ) -> CallToolResult {
        if p.region.len() < 4 {
            return res(Err("region must be [x0,y0,x1,y1]".into()));
        }
        let region = (p.region[0], p.region[1], p.region[2], p.region[3]);
        res(self.studio().doc_keyframe_move(
            &p.doc_id,
            p.layer,
            region,
            p.from_frame,
            p.to_frame,
            p.dx,
            p.dy,
            p.easing.as_deref().unwrap_or("linear"),
            p.clear_source.unwrap_or(true),
        ))
    }

    // -- pivots / palette (engine-ready sprites, cohesive colour) --
    #[tool(
        description = "Set a frame's anchor/pivot point [x,y] in document pixels (feet, weapon mount, …) so engines position the sprite. Omit `pivot` to clear it. Exported (scaled) in sheet/atlas JSON."
    )]
    async fn doc_set_pivot(&self, Parameters(p): Parameters<DocSetPivot>) -> CallToolResult {
        let pivot = match &p.pivot {
            Some(v) if v.len() >= 2 => Some([v[0], v[1]]),
            Some(_) => return res(Err("pivot must be [x,y]".into())),
            None => None,
        };
        res(self.studio().doc_set_pivot(&p.doc_id, p.frame, pivot))
    }

    #[tool(
        description = "Set a frame's collision boxes — body/hit/hurt rects an engine reads straight off the sheet. Each box is {name, kind:body|hit|hurt, rect:[x,y,w,h]}. Replaces the frame's whole set; pass boxes=[] to clear. Emitted (scaled) in sheet/atlas JSON next to pivot."
    )]
    async fn doc_set_frame_boxes(
        &self,
        Parameters(p): Parameters<DocSetFrameBoxes>,
    ) -> CallToolResult {
        let mut boxes = Vec::with_capacity(p.boxes.len());
        for b in &p.boxes {
            if b.rect.len() != 4 {
                return res(Err(format!("box '{}' rect must be [x,y,w,h]", b.name)));
            }
            boxes.push(atelier_core::document::BoxMeta {
                name: b.name.clone(),
                kind: b.kind.clone(),
                rect: [b.rect[0], b.rect[1], b.rect[2], b.rect[3]],
            });
        }
        res(self.studio().doc_set_frame_boxes(&p.doc_id, p.frame, boxes))
    }

    #[tool(
        description = "Set the document's palette: a list of [r,g,b]/[r,g,b,a] swatches. Stored on the doc and emitted in exports — lock a cohesive N-colour set across sprites."
    )]
    async fn doc_set_palette(&self, Parameters(p): Parameters<DocSetPalette>) -> CallToolResult {
        let colors: Vec<[u8; 4]> = p.colors.iter().map(|c| rgba(c)).collect();
        res(self.studio().doc_set_palette(&p.doc_id, colors))
    }

    #[tool(
        description = "Recolour a whole document in one call: swap each `from[i]` colour to `to[i]` across every cel (exact match, all channels), updating the stored palette too. `from`/`to` are equal-length lists of [r,g,b]/[r,g,b,a]. Optional `layer`/`frame` restrict scope. The recolour-variant workflow (one sprite, many palettes). Returns {changed}."
    )]
    async fn doc_palette_swap(&self, Parameters(p): Parameters<DocPaletteSwap>) -> CallToolResult {
        let from: Vec<[u8; 4]> = p.from.iter().map(|c| rgba(c)).collect();
        let to: Vec<[u8; 4]> = p.to.iter().map(|c| rgba(c)).collect();
        res(self
            .studio()
            .doc_palette_swap(&p.doc_id, from, to, p.layer, p.frame))
    }

    #[tool(
        description = "Apply MANY ordered drawing ops to one cel in a single call (fast headless editing). Each op is an object {\"op\":\"rect|line|ellipse|polyline|polygon|stroke|bezier|pencil|text|fill|replace_color|flip|shift|outline|fill_cel|clear_cel|gradient|scatter|noise|adjust|blur|quantize|symmetry|drop_shadow|glow|bevel|shade|dither|pixel_perfect\", ...} taking the same fields as the matching tool. Add per-op \"opacity\" (0..255) and/or \"blend_mode\" to composite that op instead of overwriting. Honours an active doc_select."
    )]
    async fn doc_batch(&self, Parameters(p): Parameters<DocBatch>) -> CallToolResult {
        res(self.studio().doc_batch(&p.doc_id, p.layer, p.frame, p.ops))
    }

    #[tool(
        description = "Draw ONE shape/mark on a cel — the single-op form of doc_batch (use doc_batch for many ops at once). `op` plus its flattened params: pencil{points,color,size?} · line{x0,y0,x1,y1,color,size?} · rect{x0,y0,x1,y1,color,fill?,size?} · ellipse{cx,cy,rx,ry,color,fill?} · polyline{points,color,size?,closed?} · polygon{points,color,fill?} · stroke{points,color,width?,aa?,snap?} · fill{x,y,color,tolerance?} · gradient{stops,kind?,x0,y0,x1,y1,…} · scatter{colors,x0,y0,x1,y1,density?,seed?,size?} · noise{stops,x0,y0,x1,y1,kind?,scale?,…} · text{x,y,text,color,size?} · fill_cel{color}. All also accept opacity and blend_mode. Honours an active doc_select."
    )]
    async fn doc_draw(&self, Parameters(p): Parameters<DocDraw>) -> CallToolResult {
        res(self
            .studio()
            .doc_draw(&p.doc_id, p.layer, p.frame, &p.op, p.params))
    }

    #[tool(
        description = "Apply ONE transform/effect op that REWORKS existing pixels — the complement of doc_draw (which adds marks), single-op form of doc_batch. `op` plus its flattened params, grouped: **effects** blur{radius,region?} · outline{color,aa?} · drop_shadow{color,dx?,dy?,blur?} · bevel{light,dark,depth?} · shade{light_dir?,steps?,mode?,ramp?,region?} · form{form,light_dir?,ramp?,strength?,region?} · dither{color_a,color_b,pattern?,density?,region?,only_existing?} · pixel_perfect{region?,color?}; **transform** flip{horizontal?} · shift{dx?,dy?,wrap?} · symmetry{vertical?,horizontal?,keep_left?,keep_top?}; **colour** quantize{colors,max_colors?} · replace_color{from,to,tolerance?} · adjust{hue?,sat?,lum?,region?}. All also accept opacity/blend_mode and honour an active doc_select. (Bloom-with-snap stays on doc_glow.)"
    )]
    async fn doc_fx(&self, Parameters(p): Parameters<DocFx>) -> CallToolResult {
        res(self
            .studio()
            .doc_fx(&p.doc_id, p.layer, p.frame, &p.op, p.params))
    }

    // -- world-class-art tools (the art-quality pass) --
    #[tool(
        description = "SEE a frame as an INLINE PNG (no separate file read) plus measured stats — the agent's primary and only eye for the canvas. mode: render | value/grayscale | bands | sat | hue | notan (3-value squint). grid + coords burn a pixel ruler into the upscale; onion ghosts neighbours; region crops; max_size makes a thumbnail; tile repeats the result N×N to check seamlessness; out_path also writes the PNG to a file. Stats report value min/max/mean/contrast and shadow/mid/light mass % (plus per-band coverage in bands/notan modes)."
    )]
    async fn doc_look(&self, Parameters(p): Parameters<DocLook>) -> CallToolResult {
        img_result(self.studio().look(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.scale,
            region(&p.region),
            p.mode.as_deref().unwrap_or("render"),
            p.bands.unwrap_or(4),
            p.grid.unwrap_or(false),
            p.coords.unwrap_or(false),
            p.onion.unwrap_or(false),
            p.max_size,
            p.tile,
            p.out_path.as_deref(),
        ))
    }

    #[tool(
        description = "Render the ACTIVE selection as a quick-mask overlay (selected art shown, the rest dimmed + magenta-tinted) so you never paint through an unseen mask. Returns an inline PNG + selected-pixel count and bbox."
    )]
    async fn doc_select_render(
        &self,
        Parameters(p): Parameters<DocSelectRender>,
    ) -> CallToolResult {
        img_result(self.studio().select_render(&p.doc_id, p.scale.unwrap_or(6)))
    }

    #[tool(
        description = "Document history for an all-destructive editor. action: save (snapshot the doc) | list | restore (roll back) | diff (regression deltas vs a snapshot: pixel/colour/contrast change, added/removed/recoloured) | prune. Snapshot before a risky op (form/quantize/relight/fill) and restore if it gets worse."
    )]
    async fn doc_checkpoint(&self, Parameters(p): Parameters<DocCheckpoint>) -> CallToolResult {
        res(self.studio().checkpoint(
            &p.doc_id,
            &p.action,
            p.label.as_deref(),
            p.checkpoint_id.as_deref(),
        ))
    }

    #[tool(
        description = "Snap a cel (or the whole document, if layer/frame omitted) to its locked palette by PERCEPTUALLY nearest colour (OKLab ΔE) — kills the off-palette drift that blends/dithers/glow/gradient leave behind. `palette` overrides the stored one. `alpha` controls FX bloom / AA fringe: preserve (default, RGB-only) | opaque (binarise alpha at `cutoff`, default 128 — turn a soft bloom into crisp on-palette pixels) | flatten (composite over `bg` then snap opaque). Returns the pixel count moved plus an INLINE preview of the result."
    )]
    async fn doc_snap_palette(&self, Parameters(p): Parameters<DocSnapPalette>) -> CallToolResult {
        let studio = self.studio();
        if let Some(m) = p.alpha.as_deref() {
            if !matches!(m, "preserve" | "opaque" | "flatten") {
                return res(Err(format!(
                    "unknown alpha mode '{m}' — use preserve | opaque | flatten"
                )));
            }
        }
        let alpha = alpha_snap(p.alpha.as_deref(), p.cutoff, p.bg.as_deref());
        let r = studio.snap_palette(
            &p.doc_id,
            p.layer,
            p.frame,
            p.palette.map(|v| palette_list(&v)),
            alpha,
        );
        previewed(&studio, &p.doc_id, p.layer, p.frame.unwrap_or(0), r)
    }

    #[tool(
        description = "Contiguous magic-wand → the active selection mask. Floods from (x,y) over same-colour pixels (perceptual OKLab tolerance by default; `conn8` for 8-connectivity). `layer` omitted samples the flattened composite. `mode` combines with the current selection: replace|add|subtract|intersect. The precondition for local recolour/re-shade; pair with doc_select_render to SEE the mask."
    )]
    async fn doc_select_wand(&self, Parameters(p): Parameters<DocSelectWand>) -> CallToolResult {
        res(self.studio().select_wand(
            &p.doc_id,
            p.layer,
            p.frame.unwrap_or(0),
            p.x,
            p.y,
            p.tol.unwrap_or(16),
            p.conn8.unwrap_or(false),
            p.perceptual.unwrap_or(true),
            p.mode.as_deref().unwrap_or("replace"),
        ))
    }

    #[tool(
        description = "Selective anti-aliasing (selout): drop one opaque, mid-value pixel into each outer staircase notch of the silhouette so diagonals read smooth instead of as Bresenham stairs. Pass a `ramp` to keep the AA on-palette; `max_run` keeps genuine sharp corners crisp; `only_color`/`region` scope it. Returns the AA pixel count."
    )]
    async fn doc_smooth_edges(&self, Parameters(p): Parameters<DocSmoothEdges>) -> CallToolResult {
        res(self.studio().smooth_edges(
            &p.doc_id,
            p.layer,
            p.frame,
            p.ramp.map(|v| palette_list(&v)),
            p.max_run.unwrap_or(2),
            p.keep_square.unwrap_or(true),
            p.only_color.as_deref().map(rgba),
            region(&p.region),
        ))
    }

    #[tool(
        description = "Affine-transform a cel (or `region`) IN PLACE about its centre — the #1 missing primitive: rotate degrees, scale_x/scale_y, skew_x/skew_y degrees. method rotsprite (super-sampled, keeps clusters from shattering) | nearest. preserve_volume derives scale_y=1/scale_x for squash-and-stretch. snap_palette re-snaps the transform fringe; clear_source makes it a move. Returns placed bbox + pixel counts."
    )]
    async fn doc_transform_cel(
        &self,
        Parameters(p): Parameters<DocTransformCel>,
    ) -> CallToolResult {
        let sx = p.scale_x.unwrap_or(1.0);
        let sy = match (p.preserve_volume.unwrap_or(false), p.scale_y) {
            (true, None) if sx.abs() > 1e-6 => 1.0 / sx,
            _ => p.scale_y.unwrap_or(1.0),
        };
        res(self.studio().transform_cel(
            &p.doc_id,
            p.layer,
            p.frame,
            region(&p.region),
            p.rotate.unwrap_or(0.0),
            sx,
            sy,
            p.skew_x.unwrap_or(0.0),
            p.skew_y.unwrap_or(0.0),
            p.method.as_deref().unwrap_or("rotsprite"),
            p.snap_palette.unwrap_or(true),
            p.clear_source.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Art-director scorecard: the named pixel-art failure modes the agent can't see — orphan specks, un-AA'd jaggies (outer step corners), low contrast, per-form pillow-shading and mixed light direction (via the doc_form_audit engine), value-soup massing, and off-palette drift. Verdicts are conservative (ok|warn|info) with worst-offending cells so you can fix locally. Snapshot with doc_checkpoint first if acting on it."
    )]
    async fn doc_critique(&self, Parameters(p): Parameters<DocCritique>) -> CallToolResult {
        res(self
            .studio()
            .critique(&p.doc_id, p.frame.unwrap_or(0), p.layer, region(&p.region)))
    }

    #[tool(
        description = "Multi-light form shading — key/fill/rim, the leap from one-direction `form` to PAINTED form. Reads the silhouette as a height field, derives surface normals (bulge = how domed), and lights it: key by azimuth (0=right,90=down,180=left,270=up) + elevation (0=grazing,90=head-on), an auto fill opposite the key, a Fresnel rim, and ambient. Output multiplies the base colour (hue preserved, light colour tints) or snaps to `ramp`. Honours an active selection; pass a region on multi-material sprites."
    )]
    async fn doc_relight(&self, Parameters(p): Parameters<DocRelight>) -> CallToolResult {
        let studio = self.studio();
        studio.auto_checkpoint(&p.doc_id, "relight");
        res(studio.relight(
            &p.doc_id,
            p.layer,
            p.frame,
            region(&p.region),
            p.key_azimuth.unwrap_or(315.0),
            p.key_elevation.unwrap_or(50.0),
            p.key_intensity.unwrap_or(1.0),
            p.key_color.as_deref().map(rgb3).unwrap_or([255, 255, 255]),
            p.fill_intensity.unwrap_or(0.25),
            p.fill_color.as_deref().map(rgb3).unwrap_or([120, 140, 200]),
            p.rim_intensity.unwrap_or(0.0),
            p.rim_color.as_deref().map(rgb3).unwrap_or([255, 255, 255]),
            p.ambient.unwrap_or(0.35),
            p.ambient_color
                .as_deref()
                .map(rgb3)
                .unwrap_or([120, 130, 170]),
            p.bulge.unwrap_or(0.8),
            p.ramp.map(|v| palette_list(&v)),
        ))
    }

    #[tool(
        description = "Graduated multi-tone dithering across a whole RAMP along an axis (h|v|radial) — master gradient shading, vs the two-colour `dither`. pattern bayer2/4/8 | checker | ign (blue-noise, no visible matrix grid). only_existing repaints just opaque pixels (shade existing art, keep alpha). Honours an active selection. Snap afterwards with doc_snap_palette if it drifts."
    )]
    async fn doc_dither_ramp(&self, Parameters(p): Parameters<DocDitherRamp>) -> CallToolResult {
        res(self.studio().dither_ramp(
            &p.doc_id,
            p.layer,
            p.frame,
            region(&p.region),
            palette_list(&p.ramp),
            p.axis.as_deref().unwrap_or("v"),
            p.pattern.as_deref().unwrap_or("bayer4"),
            p.only_existing.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Every frame in ONE labelled inline grid (index + duration) — the animator's flip-test the agent can't otherwise do. onion=true ghosts each cell's previous frame under it (per-pair onion skin — judge spacing/overlap/popping from a single image). cols sets the grid width; scale upscales each frame."
    )]
    async fn doc_contact_sheet(
        &self,
        Parameters(p): Parameters<DocContactSheet>,
    ) -> CallToolResult {
        img_result(self.studio().contact_sheet(
            &p.doc_id,
            p.scale.unwrap_or(4),
            p.cols.unwrap_or(8),
            p.onion.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Generate a cohesive palette in OKLCh — ONE generator for a single shading ramp (scheme=\"mono\") or a multi-hue scheme (complementary|triadic|analogous|split|tetradic). `count` colours per ramp, `hue_shift` warms light / cools shadow across each ramp, `sat_curve` (flat|arc|sat-in-shadow), `anchor_midtone` pins the base at the mid step. Returns ramps + flat palette + hex + evenness validation; `set_doc` locks it on a document. Supersedes palette_ramp / doc_make_perceptual_ramp / doc_harmony_palette."
    )]
    async fn doc_palette(&self, Parameters(p): Parameters<DocPalette>) -> CallToolResult {
        res(self.studio().palette(
            rgba(&p.base),
            p.scheme.as_deref().unwrap_or("mono"),
            p.count.unwrap_or(5),
            p.value_lo,
            p.value_hi,
            p.hue_shift.unwrap_or(20.0),
            p.sat_curve.as_deref().unwrap_or("arc"),
            p.anchor_midtone.unwrap_or(false),
            p.set_doc.as_deref(),
        ))
    }

    #[tool(
        description = "Draw a shaded isometric cuboid (top + two side faces) from one base colour, auto-shaded along a perceptual ramp — the hard-surface form primitive `form` can't make (crates, blocks, buildings, dice). (cx,cy) = centre of the top diamond, s = its half-width, ht = body height; light_right brightens the right face."
    )]
    async fn doc_box(&self, Parameters(p): Parameters<DocBox>) -> CallToolResult {
        res(self.studio().box_iso(
            &p.doc_id,
            p.layer,
            p.frame,
            p.cx,
            p.cy,
            p.s,
            p.ht,
            rgba(&p.color),
            p.light_right.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Add a faint, non-destructive guide layer to construct against, then delete with doc_layer op=delete. kind: thirds (rule-of-thirds) | grid (square, `spacing`) | iso (2:1 lattice) | vp (rays from a vanishing point `vp`=[x,y]). Pure construction scaffolding — perspective, iso, and composition."
    )]
    async fn doc_perspective_guide(
        &self,
        Parameters(p): Parameters<DocPerspectiveGuide>,
    ) -> CallToolResult {
        let vp = p.vp.as_ref().filter(|v| v.len() >= 2).map(|v| (v[0], v[1]));
        res(self.studio().perspective_guide(
            &p.doc_id,
            p.kind.as_deref().unwrap_or("thirds"),
            p.color.as_deref().map(rgba).unwrap_or([255, 0, 255, 130]),
            p.spacing.unwrap_or(8),
            vp,
        ))
    }

    #[tool(
        description = "Form-following selective outline (vs a flat black keyline): mode from_fill colours each silhouette edge from the fill it borders, shaded `steps` darker/lighter; light/dark bias the whole contour. `ramp` keeps it on-palette. The 'painted' contour that turns with the form."
    )]
    async fn doc_outline_selective(
        &self,
        Parameters(p): Parameters<DocOutlineSelective>,
    ) -> CallToolResult {
        res(self.studio().outline_selective(
            &p.doc_id,
            p.layer,
            p.frame,
            p.mode.as_deref().unwrap_or("from_fill"),
            p.ramp.map(|v| palette_list(&v)),
            p.steps.unwrap_or(2),
            region(&p.region),
        ))
    }

    #[tool(
        description = "Paint a procedural MATERIAL onto the opaque pixels of a cel from one base colour: metal (specular band + reflection), wood (grain), stone (mottle + speckle), water (ripples), cloth (weave), skin (soft gradient), glass (sheen + streak). Deterministic in `seed`; pass `ramp` to control the palette, or `region`/an active selection to clip it. Turns 6–10 blind calls into 'reads as the material'."
    )]
    async fn doc_material(&self, Parameters(p): Parameters<DocMaterial>) -> CallToolResult {
        let studio = self.studio();
        studio.auto_checkpoint(&p.doc_id, "material");
        res(studio.material(
            &p.doc_id,
            p.layer,
            p.frame,
            region(&p.region),
            &p.material,
            rgba(&p.color),
            p.seed.unwrap_or(1),
            p.ramp.map(|v| palette_list(&v)),
        ))
    }

    #[tool(
        description = "Translucency report — makes glass/glow/soft-FX alpha MEASURABLE instead of eyeballed. Over the flattened frame (or one layer, region-clipped): counts opaque/partial/transparent pixels, mean alpha of non-transparent pixels, a partial-alpha band histogram, and the bbox of the partial pixels."
    )]
    async fn doc_translucency_report(
        &self,
        Parameters(p): Parameters<DocTranslucency>,
    ) -> CallToolResult {
        res(self.studio().doc_translucency_report(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            region(&p.region),
        ))
    }

    #[tool(
        description = "Draw a HUD/UI panel: filled body + border + optional inner bevel (top/left lit, bottom/right shadowed) — a ready dialog/HUD box. Pairs with doc_draw op=text for labels."
    )]
    async fn doc_panel(&self, Parameters(p): Parameters<DocPanel>) -> CallToolResult {
        res(self.studio().panel(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x,
            p.y,
            p.w,
            p.h,
            rgba(&p.fill),
            p.border.as_deref().map(rgba).unwrap_or([20, 20, 28, 255]),
            p.bevel.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Paint a whole region DECLARATIVELY from a character grid (the inverse of doc_dump_region): `legend` maps single characters to [r,g,b(,a)] colours or integer PALETTE INDICES, `rows` are pixel-row strings ('.'/' ' leave the pixel untouched). Emitting a sprite as a grid eliminates the absolute-coordinate failure class — prefer this over long pencil/rect sequences for detailed shapes. Verify by diffing against doc_dump_region. Returns painted/clipped counts plus an INLINE preview. Honours an active selection."
    )]
    async fn doc_paint_grid(&self, Parameters(p): Parameters<DocPaintGrid>) -> CallToolResult {
        let studio = self.studio();
        let r = studio.doc_paint_grid(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x.unwrap_or(0),
            p.y.unwrap_or(0),
            p.legend,
            p.rows,
        );
        previewed(&studio, &p.doc_id, Some(p.layer), p.frame, r)
    }

    // -- part-layer rig: limb animation as transforms, not repaints --
    #[tool(
        description = "RIG step: cut a part (arm, head, tail) of a flat sprite onto its OWN new layer directly above, same coordinates — by `region` rect or the active selection (doc_select_wand the limb, then use_selection=true). frames=\"all\" cuts every frame so the part stays separated across the timeline. After extraction, animate the part with doc_keyframe_transform / doc_keyframe_move on ITS layer while the body stays untouched. Returns the new layer index."
    )]
    async fn doc_extract_to_layer(
        &self,
        Parameters(p): Parameters<DocExtractToLayer>,
    ) -> CallToolResult {
        res(self.studio().doc_extract_to_layer(
            &p.doc_id,
            p.layer,
            p.frame.unwrap_or(0),
            region(&p.region),
            p.use_selection.unwrap_or(false),
            p.name,
            p.frames.as_deref() == Some("all"),
        ))
    }

    #[tool(
        description = "JOINT-rotate a part across frames in ONE call: reads `region` from from_frame, then every frame in (from_frame, to_frame] gets the part rotated by the eased fraction of `rot_deg` about `pivot` (the joint, e.g. the shoulder — document pixels) plus the eased (dx,dy), original region cleared first. THE replacement for blind per-frame limb repainting — 'swing the arm 30° about the shoulder over frames 1-4' is one call. Works best on a part layer from doc_extract_to_layer. Resampled pixels snap to the locked palette by default."
    )]
    async fn doc_keyframe_transform(
        &self,
        Parameters(p): Parameters<DocKeyframeTransform>,
    ) -> CallToolResult {
        if p.region.len() < 4 {
            return res(Err("region must be [x0,y0,x1,y1]".into()));
        }
        if p.pivot.len() < 2 {
            return res(Err("pivot must be [x,y]".into()));
        }
        res(self.studio().doc_keyframe_transform(
            &p.doc_id,
            p.layer,
            (p.region[0], p.region[1], p.region[2], p.region[3]),
            (p.pivot[0], p.pivot[1]),
            p.from_frame,
            p.to_frame,
            p.rot_deg.unwrap_or(0.0),
            p.dx.unwrap_or(0),
            p.dy.unwrap_or(0),
            p.easing.as_deref().unwrap_or("linear"),
            p.snap_palette.unwrap_or(true),
        ))
    }

    // -- reference subsystem: recreate-from-sample as a measurable loop --
    #[tool(
        description = "Bring an external image into the reference workflow. `op`: set — attach the ORIGINAL reference (the sample being recreated; `path`, omit to clear) so doc_ref_analyze/doc_ref_compare can score likeness; returns aspect-true fit suggestions. import — trace a source image cleaned onto a guide layer: `path`, `target_w` (required), optional `target_h` (omit = aspect-true), `colors`, `dither`, `defringe`, `to_doc_palette`, `remove_bg` (corner-seeded bg flood), `pin` (colours to keep). Returns an inline preview."
    )]
    async fn doc_ref(&self, Parameters(p): Parameters<DocRefOp>) -> CallToolResult {
        let studio = self.studio();
        match p.op.as_str() {
            "set" => res(studio.set_reference(&p.doc_id, p.path.as_deref())),
            "import" => {
                let Some(path) = p.path.as_deref() else {
                    return res(Err("doc_ref op=import needs `path`".to_string()));
                };
                let Some(target_w) = p.target_w else {
                    return res(Err("doc_ref op=import needs `target_w`".to_string()));
                };
                let (layer, frame) = (p.layer.unwrap_or(0), p.frame.unwrap_or(0));
                let r = studio.import_clean(
                    &p.doc_id,
                    layer,
                    frame,
                    path,
                    target_w,
                    p.target_h,
                    p.colors.unwrap_or(16),
                    p.dither,
                    p.defringe.unwrap_or(false),
                    p.to_doc_palette.unwrap_or(false),
                    p.remove_bg.unwrap_or(false),
                    p.pin.as_ref().map(|v| palette_list(v)).unwrap_or_default(),
                );
                previewed(&studio, &p.doc_id, Some(layer), frame, r)
            }
            other => res(Err(format!(
                "doc_ref: unknown op '{other}' — use set|import"
            ))),
        }
    }

    #[tool(
        description = "VIEW and decompose the document's reference image (inline PNG): background coverage, a frequency-weighted SUBJECT palette to lock with doc_set_palette, and the subject's silhouette as a text grid at target size — the scaffolding to redraw deliberately instead of freehanding from memory. `path` analyzes an external file instead; `target_w` plans at a different size."
    )]
    async fn doc_ref_analyze(&self, Parameters(p): Parameters<DocRefAnalyze>) -> CallToolResult {
        img_result(self.studio().ref_analyze(
            &p.doc_id,
            p.path.as_deref(),
            p.target_w,
            p.colors.unwrap_or(8),
        ))
    }

    #[tool(
        description = "SCORE a frame against the stored reference — run this after every drawing pass when recreating from a sample. Returns an inline side-by-side (mode=\"overlay\" ghosts the reference under the art for proportion checks), silhouette IoU (≥0.80 = shape reads), per-cell OKLab ΔE with the worst cells as canvas rects (fix those first), and reference colours missing from the palette. Iterate until iou/mean_delta stop improving."
    )]
    async fn doc_ref_compare(&self, Parameters(p): Parameters<DocRefCompare>) -> CallToolResult {
        img_result(self.studio().ref_compare(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.mode.as_deref().unwrap_or("side_by_side"),
            p.cells.unwrap_or(8),
        ))
    }

    #[tool(
        description = "PER-PIXEL signed error map vs the stored reference — the see-and-repair eye for the last 5% that ref_compare's aggregate ΔE can't show. Returns a HEAT png (red = canvas too light, blue = too dark, green = wrong colour; brightness = ΔE) plus the `top` worst INDIVIDUAL pixels, each with x,y, ΔE, and a fix direction (lighten/darken + saturate/desaturate + shift hue). Run after a pass, fix the named pixels, re-run."
    )]
    async fn doc_diff_map(&self, Parameters(p): Parameters<DocDiffMap>) -> CallToolResult {
        img_result(
            self.studio()
                .diff_map(&p.doc_id, p.frame.unwrap_or(0), p.top.unwrap_or(20)),
        )
    }

    #[tool(
        description = "AI eye for FREE-FORM art — what diff_map can't do without a reference. Renders the frame and asks the MCP HOST to run its own vision model over it (atelier ships no weights, makes no network call, holds no keys — the host samples; nothing leaves your client beyond that). Returns a structured critique: does the silhouette read, are proportions/anatomy right, is the value/colour structure working, what 3 fixes raise it most. `focus` weights one axis. Requires a host that advertises the `sampling` capability; errors clearly if it doesn't."
    )]
    async fn doc_critique_vision(
        &self,
        Parameters(p): Parameters<DocCritiqueVision>,
        peer: rmcp::Peer<RoleServer>,
    ) -> CallToolResult {
        // Fail fast if the client never advertised sampling — otherwise
        // create_message would block waiting for a response a non-sampling host
        // never sends.
        let supports_sampling = peer
            .peer_info()
            .map(|i| i.capabilities.sampling.is_some())
            .unwrap_or(false);
        if !supports_sampling {
            return res(Err(
                "vision critique unavailable — this MCP host does not advertise \
                the `sampling` capability, so it cannot run a vision model on the render. \
                (atelier ships no model and makes no network call; the host must sample.)"
                    .to_string(),
            ));
        }

        let frame = p.frame.unwrap_or(0);
        let scale = p.scale.unwrap_or(8).clamp(1, 16);
        // Render to owned bytes; the studio guard drops before the await below so
        // the lock is never held across the sampling round-trip.
        let png = match self.studio().render_png_bytes(&p.doc_id, frame, scale) {
            Ok(bytes) => bytes,
            Err(e) => return res(Err(e)),
        };

        let focus = match p.focus.as_deref().map(str::trim) {
            Some(f) if !f.is_empty() => format!(" Weight the critique toward: {f}."),
            _ => String::new(),
        };
        let system = "You are a master pixel-art director reviewing a single sprite/frame. \
            Be specific and honest — name what is wrong and where (use rough pixel regions \
            like 'upper-left', 'the head'), not vague praise. Pixel art is low-resolution on \
            purpose; judge it as such (silhouette readability, value grouping, palette \
            discipline, proportion/anatomy, appeal), not as a photo."
            .to_string();
        let instruction = format!(
            "Critique this pixel-art frame.{focus}\n\nReturn:\n\
             1. Reads-as: what the subject clearly is, or 'unclear' + why.\n\
             2. Silhouette & proportion: what's off.\n\
             3. Value & colour: flat/muddy/banding/off-palette issues.\n\
             4. Top 3 fixes, highest-impact first, each naming where on the canvas.\n\
             Keep it tight."
        );

        let image = SamplingMessageContent::Image(RawImageContent {
            data: base64(&png),
            mime_type: "image/png".to_string(),
            meta: None,
        });
        let text = SamplingMessageContent::text(instruction);
        let mut params = CreateMessageRequestParams::default();
        params.messages = vec![SamplingMessage::new_multiple(Role::User, vec![image, text])];
        params.system_prompt = Some(system);
        params.max_tokens = 1024;

        match peer.create_message(params).await {
            Ok(result) => {
                let critique = result
                    .message
                    .content
                    .iter()
                    .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string();
                if critique.is_empty() {
                    return res(Err(format!(
                        "the host's vision model returned no text (model: {})",
                        result.model
                    )));
                }
                res(Ok(json!({
                    "doc_id": p.doc_id,
                    "frame": frame,
                    "model": result.model,
                    "critique": critique,
                })))
            }
            Err(e) => res(Err(format!(
                "vision critique unavailable — the MCP host must advertise the `sampling` \
                 capability (it runs its own vision model; atelier sends nothing elsewhere). \
                 host: {e}"
            ))),
        }
    }

    #[tool(
        description = "Generate a radial FX animation (ring | disc | rays) expanding from (cx,cy) across `frames`, fading along a ramp, tagged `burst` — impacts, shockwaves, explosions as frames. Clears the target layer's cels. Export with doc_export op=anim tag=burst."
    )]
    async fn doc_burst(&self, Parameters(p): Parameters<DocBurst>) -> CallToolResult {
        res(self.studio().burst(
            &p.doc_id,
            p.layer.unwrap_or(0),
            p.cx,
            p.cy,
            p.frames.unwrap_or(6),
            p.max_radius,
            p.kind.as_deref().unwrap_or("ring"),
            rgba(&p.color),
            p.ramp.map(|v| palette_list(&v)),
        ))
    }

    #[tool(
        description = "Build a CONNECTED humanoid figure from named JOINT coordinates — reason in joint space (which you do well) instead of placing every silhouette vertex (which you don't). Each bone is drawn as a tapered capsule (a doc_draw op=stroke ribbon) sharing its endpoints, so the whole body is ONE connected silhouette by construction: no detached limbs, no blocky rect stacks. joints = {\"head\":[x,y],\"shoulder_l\":[x,y],\"shoulder_r\":[x,y],\"elbow_l\":...,\"elbow_r\":...,\"hand_l\":...,\"hand_r\":...,\"hip_l\":...,\"hip_r\":...,\"knee_l\":...,\"knee_r\":...,\"foot_l\":...,\"foot_r\":...} (chest/pelvis derived from the midpoints). Re-pose across frames by calling again with new joints — the base for non-wobbly animation. Tune limb_w/torso_w/head_r to the sprite size."
    )]
    async fn doc_figure(&self, Parameters(p): Parameters<DocFigure>) -> CallToolResult {
        let joints: std::collections::HashMap<String, (i32, i32)> = p
            .joints
            .iter()
            .filter(|(_, v)| v.len() >= 2)
            .map(|(k, v)| (k.clone(), (v[0] as i32, v[1] as i32)))
            .collect();
        res(self.studio().figure(
            &p.doc_id,
            p.layer,
            p.frame,
            &joints,
            rgba(&p.color),
            p.limb_w.unwrap_or(3) as i32,
            p.torso_w.unwrap_or(6) as i32,
            p.head_r.unwrap_or(4) as i32,
            p.aa.unwrap_or(true),
            p.snap.unwrap_or(true),
        ))
    }

    #[tool(
        description = "GENERATE a side-view walk cycle from a base standing pose (the same 13 joints as doc_figure). Feet stride along a gait path (one planted, one swinging, half a cycle apart), knees/elbows are solved by 2-bone IK, arms counter-swing the legs, and the body bobs — each frame is drawn as the connected-capsule figure and the range is tagged \"walk\". The walk is generated from joints, not hand-painted frame-by-frame, so limbs never wobble or detach. Tune frames/stride/lift/bob/arm_swing. Export with doc_export op=anim tag=walk."
    )]
    async fn doc_walk(&self, Parameters(p): Parameters<DocWalk>) -> CallToolResult {
        let joints: std::collections::HashMap<String, (i32, i32)> = p
            .joints
            .iter()
            .filter(|(_, v)| v.len() >= 2)
            .map(|(k, v)| (k.clone(), (v[0] as i32, v[1] as i32)))
            .collect();
        res(self.studio().walk(
            &p.doc_id,
            p.layer,
            &joints,
            p.frames.unwrap_or(8),
            p.stride.unwrap_or(10) as i32,
            p.lift.unwrap_or(4) as i32,
            p.bob.unwrap_or(1) as i32,
            p.arm_swing.unwrap_or(6) as i32,
            rgba(&p.color),
            p.limb_w.unwrap_or(3) as i32,
            p.torso_w.unwrap_or(6) as i32,
            p.head_r.unwrap_or(4) as i32,
            p.aa.unwrap_or(true),
            p.snap.unwrap_or(true),
        ))
    }

    #[tool(
        description = "GENERATE a full animation cycle for a named GAIT from one standing pose (the same 13 joints as doc_figure) — the moveset generator. `gait`: idle (breathing bob) | run (airborne stride, pumping arms, forward lean) | jump (crouch → rise+tuck → fall → landing absorb) | attack (lead-arm sweep with a lunge) | hurt (recoil and recover). Knees/elbows are solved by 2-bone IK and every frame is the connected-capsule figure, so limbs never wobble or detach. Amplitudes scale from the figure's own leg length × `intensity`, so presets fit any sprite size. Frames are tagged with the gait — one call per gait builds a whole character moveset from the SAME pose (walk has its own tool). Export each with doc_export op=anim tag=<gait>."
    )]
    async fn doc_pose_cycle(&self, Parameters(p): Parameters<DocPoseCycle>) -> CallToolResult {
        let joints: std::collections::HashMap<String, (i32, i32)> = p
            .joints
            .iter()
            .filter(|(_, v)| v.len() >= 2)
            .map(|(k, v)| (k.clone(), (v[0] as i32, v[1] as i32)))
            .collect();
        res(self.studio().pose_cycle(
            &p.doc_id,
            p.layer,
            &joints,
            &p.gait,
            p.frames.unwrap_or(0),
            p.intensity.unwrap_or(1.0),
            rgba(&p.color),
            p.limb_w.unwrap_or(3) as i32,
            p.torso_w.unwrap_or(6) as i32,
            p.head_r.unwrap_or(4) as i32,
            p.aa.unwrap_or(true),
            p.snap.unwrap_or(true),
        ))
    }
}

/// The default ("core") tool profile — the ~30 tools the canonical sprite /
/// animation / tile / game-set / recreate-from-reference workflows need. The
/// full surface (extra effects, rigging, audits, library exports) is advertised when
/// `ATELIER_PROFILE=full`. The profile filters only what `tools/list` ADVERTISES;
/// every tool still EXECUTES via call_tool (so `atelier replay` and a flag-flip
/// both reach the long tail).
const CORE_TOOLS: &[&str] = &[
    // lifecycle + the eye
    "doc_create",
    "doc_info",
    "list_docs",
    "delete_doc",
    "doc_look",
    "doc_checkpoint",
    // draw / transform
    "doc_draw",
    "doc_batch",
    "doc_fx",
    "doc_paint_grid",
    // structure
    "doc_layer",
    "doc_frame",
    "doc_region",
    "doc_select",
    "doc_add_tag",
    // palette
    "doc_palette",
    "doc_set_palette",
    "doc_palette_swap",
    "doc_snap_palette",
    // export
    "doc_export",
    // most-used audits (readers)
    "doc_critique",
    "doc_silhouette",
    "doc_components",
    "doc_palette_report",
    "doc_contrast_check",
    "doc_frame_diff",
    // reference loop
    "doc_ref",
    "doc_ref_compare",
    // the game layer — a game is a SET of documents, not one
    "doc_set_audit",
    "doc_set_palette_sync",
];

/// True when the full tool surface should be advertised (`ATELIER_PROFILE=full`).
fn profile_full() -> bool {
    std::env::var("ATELIER_PROFILE")
        .map(|v| v.eq_ignore_ascii_case("full"))
        .unwrap_or(false)
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Atelier {
    /// Advertise the active tool profile: the ~28 `CORE_TOOLS` by default, or the
    /// full surface when `ATELIER_PROFILE=full`. Discovery filter only — every
    /// tool still executes via `call_tool`, so recipes/replay reach the tail.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = self.tool_router.list_all();
        if !profile_full() {
            tools.retain(|t| CORE_TOOLS.contains(&t.name.as_ref()));
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    /// Hand-written so we can record each call before delegating to the
    /// `#[tool_router]`-generated dispatcher. This replicates the body the
    /// `#[tool_handler]` macro would otherwise emit (build a `ToolCallContext`,
    /// then `self.tool_router.call(...)`); because we define it, the macro skips
    /// its own. Application errors come back as Ok `{"error": ...}` payloads (see
    /// `res`), so we record only steps that actually succeeded — a failed step
    /// would break `atelier replay`, which fails fast on an error payload.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        // Snapshot tool name + raw args up front (the request is moved into the
        // dispatcher). args default to `{}` to match a recipe step's shape.
        let recording = self.recorder.as_ref().map(|r| {
            let args = request
                .arguments
                .clone()
                .map(Value::Object)
                .unwrap_or_else(|| json!({}));
            (r.clone(), request.name.to_string(), args)
        });
        // Snapshot the live-feed pieces before the request moves: tool name,
        // compact (length-capped) args, and the args doc_id if present. The doc id
        // may instead come from the RESULT (doc_create names the new doc) — that's
        // resolved after the call, so freshly created docs still show in the feed.
        let feed = self.change_tx.as_ref().map(|_| {
            let m = request.arguments.as_ref();
            let args_doc = m
                .and_then(|m| m.get("doc_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            // Compact, length-capped args so a fat call (point lists, base64)
            // can't bloat the SSE frame fanned out to every browser.
            let mut args = m
                .map(|m| serde_json::to_string(&Value::Object(m.clone())).unwrap_or_default())
                .unwrap_or_default();
            const MAX: usize = 280;
            if args.chars().count() > MAX {
                args = args.chars().take(MAX).collect::<String>();
                args.push('…');
            }
            (request.name.to_string(), args, args_doc)
        });
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await?;
        if let Some((recorder, tool, args)) = recording {
            if !is_error_result(&result) {
                recorder.record(&tool, args);
            }
        }
        if let (Some(tx), Some((tool, args, args_doc))) = (&self.change_tx, &feed) {
            if !is_error_result(&result) {
                if let Some(doc) = args_doc.clone().or_else(|| result_doc_id(&result)) {
                    let _ = tx.send(json!({ "doc": doc, "tool": tool, "args": args }).to_string());
                }
            }
        }
        Ok(result)
    }

    /// List the browsable resources: structure JSON + frame-0 render per document.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(
            self.list_resource_specs(),
        ))
    }

    /// Read one resource by URI. Unknown URIs and missing documents become a
    /// `resource_not_found` error rather than a panic.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let target = parse_resource_uri(&request.uri).ok_or_else(|| {
            ErrorData::resource_not_found(format!("unknown resource uri: {}", request.uri), None)
        })?;
        let contents = self
            .fetch_resource(&request.uri, target)
            .map_err(|e| ErrorData::resource_not_found(e, None))?;
        Ok(ReadResourceResult::new(vec![contents]))
    }

    /// List the packaged workflow prompts (the draw -> render -> audit loop).
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(prompt_specs()))
    }

    /// Fill one workflow prompt from its arguments. Unknown names become an
    /// `invalid_params` error.
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        let (text, description) =
            build_prompt(&request.name, &request.arguments).ok_or_else(|| {
                ErrorData::invalid_params(format!("unknown prompt: {}", request.name), None)
            })?;
        let message = PromptMessage::new_text(PromptMessageRole::User, text);
        Ok(GetPromptResult::new(vec![message]).with_description(description))
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        info.instructions = Some(
            "atelier: the pixel-art studio you can see — a headless editor where every \
             mark is a tool call and doc_look hands the frame back as an image. doc_create a \
             layered/animated document, then paint cels with doc_draw (one op: \
             line/rect/ellipse/fill/stroke/text/…) or doc_batch (many ops in one call). LOOK with doc_look \
             after every burst of edits — it returns the frame as an INLINE image plus \
             value stats (pass doc_look out_path when you also need the PNG written to a file). \
             Recreating from a reference image? doc_ref op=set FIRST, doc_ref_analyze \
             to plan (subject palette + silhouette), optionally doc_ref op=import onto a \
             guide layer, then doc_ref_compare after EVERY pass — it scores silhouette \
             IoU and per-cell colour ΔE against the reference so likeness is measured, \
             not remembered. \
             Audit before exporting: doc_critique (failure modes), doc_palette_report, \
             doc_silhouette. Animate by duplicating frames (doc_frame op=add copy_from) \
             and editing what moves — doc_keyframe_move for eased motion; doc_dissolve is \
             a dissolve, NOT pose interpolation. doc_checkpoint save before risky ops \
             (tween/form/quantize/relight) — restore rolls back. Export with \
             doc_export (op=sheet|anim|tileset) / export_all. list_docs browses the \
             library. This is the CORE tool profile (~28 tools); the full surface \
             (extra effects like relight/material/rim_light, rigging, audits, \
             perspective/wang/atlas) is available by restarting with \
             ATELIER_PROFILE=full."
                .into(),
        );
        info
    }
}

/// Run over stdio (default transport). `record` enables session recording to a
/// recipe at that path.
pub async fn run(record: Option<std::path::PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let mut atelier = Atelier::new();
    if let Some(path) = record {
        atelier = atelier.with_recording(path);
    }
    let service = atelier.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Clamp a render scale to the 1..=16 the gallery serves (0/absent -> default 4).
fn gallery_scale(scale: Option<u32>) -> u32 {
    scale.unwrap_or(4).clamp(1, 16)
}

/// The whole live gallery page, self-contained (no external assets). Cards are
/// filled and refreshed by the inline script from `/gallery` + the render route.
const GALLERY_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>atelier gallery</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; padding: 1.5rem; background: #14161a; color: #e6e8ec;
         font: 14px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace; }
  h1 { font-size: 1.1rem; font-weight: 600; margin: 0 0 1rem; }
  #grid { display: grid; gap: 1rem;
          grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); }
  .card { background: #1d2026; border: 1px solid #2a2e36; border-radius: 8px;
          padding: .75rem; }
  .card img { width: 100%; height: auto; display: block; border-radius: 4px;
              background: #0c0d10; image-rendering: pixelated; }
  .name { margin: .5rem 0 .15rem; font-weight: 600; word-break: break-all; }
  .meta { color: #8b919c; font-size: 12px; }
  #empty { color: #8b919c; }
</style>
</head>
<body>
<h1>atelier &middot; live gallery</h1>
<div id="grid"></div>
<p id="empty" hidden>No documents yet. Create one and it will appear here.</p>
<script>
const grid = document.getElementById('grid');
const empty = document.getElementById('empty');
function imgUrl(id) { return `/gallery/${encodeURIComponent(id)}/render.png?frame=0&scale=4&t=${Date.now()}`; }
async function load() {
  let data;
  try { data = await (await fetch('/gallery/docs')).json(); } catch (e) { return; }
  const docs = data.documents || [];
  empty.hidden = docs.length > 0;
  const seen = new Set();
  for (const d of docs) {
    seen.add(d.id);
    let card = document.getElementById('c-' + d.id);
    if (!card) {
      card = document.createElement('div');
      card.className = 'card';
      card.id = 'c-' + d.id;
      card.innerHTML = '<img alt=""><div class="name"></div><div class="meta"></div>';
      grid.appendChild(card);
    }
    card.querySelector('.name').textContent = d.name;
    card.querySelector('.meta').textContent =
      `${d.w}x${d.h} · ${d.frames}f · ${d.layers}L`;
    if (!card.querySelector('img').src) card.querySelector('img').src = imgUrl(d.id);
  }
  for (const card of [...grid.children]) {
    if (!seen.has(card.id.slice(2))) card.remove();
  }
}
function refreshImages() {
  for (const card of grid.children) card.querySelector('img').src = imgUrl(card.id.slice(2));
}
function refreshOne(id) {
  const card = document.getElementById('d-' + id);
  if (card) card.querySelector('img').src = imgUrl(id); else load();
}
load();
// Live: the server pushes the changed doc_id on each successful tool call —
// re-render just that card instantly. Polls stay as a fallback.
try {
  const es = new EventSource('/gallery/events');
  es.onmessage = (e) => { try { refreshOne(JSON.parse(e.data).doc); } catch (_) {} };
} catch (err) {}
setInterval(refreshImages, 5000);
setInterval(load, 10000);
</script>
</body>
</html>"#;

/// The native interactive playground: a self-contained page that speaks MCP to
/// `/mcp` (the same endpoint clients use) — auto-builds a form per tool from its
/// JSON schema, runs it, shows the inline image + JSON result, and renders the
/// live canvas. No Node, no external assets.
const PLAYGROUND_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>atelier playground</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin: 0; height: 100vh; display: grid; grid-template-columns: 240px 1fr 1fr;
         background: #14161a; color: #e6e8ec; overflow: hidden;
         font: 13px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace; }
  .col { height: 100vh; overflow-y: auto; padding: .75rem; border-right: 1px solid #2a2e36; }
  h1 { font-size: 1rem; margin: 0 0 .5rem; }
  h2 { font-size: .85rem; color: #8b919c; margin: 0 0 .5rem; text-transform: uppercase; letter-spacing: .05em; }
  input, textarea, select, button { font: inherit; }
  #filter { width: 100%; padding: .4rem; margin-bottom: .5rem; background: #0c0d10;
            border: 1px solid #2a2e36; border-radius: 5px; color: #e6e8ec; }
  .tool { display: block; width: 100%; text-align: left; padding: .35rem .5rem; margin: 1px 0;
          background: none; border: none; border-radius: 5px; color: #c9cdd4; cursor: pointer; word-break: break-all; }
  .tool:hover { background: #1d2026; }
  .tool.sel { background: #2a3550; color: #fff; }
  .field { margin: .5rem 0; }
  .field label { display: block; color: #aeb3bd; margin-bottom: .15rem; }
  .field .req { color: #e0a0a0; }
  .field .desc { color: #6f7681; font-size: 11px; margin-bottom: .2rem; }
  .field input[type=text], .field input[type=number], .field textarea, .field select {
    width: 100%; padding: .35rem; background: #0c0d10; border: 1px solid #2a2e36;
    border-radius: 5px; color: #e6e8ec; }
  .field textarea { min-height: 3rem; resize: vertical; }
  #run { margin-top: .6rem; padding: .5rem 1rem; background: #2f6f4f; color: #fff;
         border: none; border-radius: 6px; cursor: pointer; font-weight: 600; }
  #run:hover { background: #3a8a62; }
  #status { margin-left: .6rem; color: #8b919c; }
  #result img, #canvas { max-width: 100%; image-rendering: pixelated; background: #0c0d10;
                         border: 1px solid #2a2e36; border-radius: 6px; display: block; }
  pre { white-space: pre-wrap; word-break: break-word; background: #0c0d10; border: 1px solid #2a2e36;
        border-radius: 6px; padding: .5rem; max-height: 40vh; overflow: auto; color: #c9cdd4; }
  .err { color: #e0a0a0; }
  #stage { position: relative; display: inline-block; max-width: 100%; line-height: 0; }
  #overlay { position: absolute; inset: 0; width: 100%; height: 100%; cursor: crosshair;
             touch-action: none; image-rendering: pixelated; }
  #drawbar { display: flex; flex-wrap: wrap; gap: .35rem; align-items: center; margin: .25rem 0 .5rem; }
  #drawbar button { padding: .3rem .5rem; background: #1d2026; color: #c9cdd4; border: 1px solid #2a2e36;
                    border-radius: 5px; cursor: pointer; }
  #drawbar button.sel { background: #2a3550; color: #fff; border-color: #3a4a72; }
  #drawbar label { color: #aeb3bd; display: flex; align-items: center; gap: .25rem; }
  #drawbar input[type=number] { width: 3.4rem; padding: .25rem; background: #0c0d10;
                                border: 1px solid #2a2e36; border-radius: 5px; color: #e6e8ec; }
  #drawbar input[type=color] { width: 2rem; height: 1.7rem; padding: 0; background: none;
                               border: 1px solid #2a2e36; border-radius: 5px; }
</style>
</head>
<body>
<div class="col" id="left">
  <h1>atelier</h1>
  <input id="filter" placeholder="filter tools...">
  <div id="tools"></div>
</div>
<div class="col" id="mid">
  <h2 id="toolName">pick a tool</h2>
  <div id="toolDesc" class="desc" style="color:#8b919c;margin-bottom:.5rem"></div>
  <form id="form" onsubmit="return false"></form>
  <button id="run" hidden>Run</button><span id="status"></span>
</div>
<div class="col" id="right" style="border-right:none">
  <h2>canvas — draw = tool calls</h2>
  <select id="docsel" style="width:100%;padding:.35rem;background:#0c0d10;border:1px solid #2a2e36;border-radius:5px;color:#e6e8ec;margin-bottom:.5rem"></select>
  <div id="drawbar">
    <button data-dt="pencil" class="sel">pencil</button>
    <button data-dt="line">line</button>
    <button data-dt="rect">rect</button>
    <button data-dt="ellipse">ellipse</button>
    <button data-dt="fill">fill</button>
    <button data-dt="eraser">eraser</button>
    <label title="rect/ellipse filled"><input type="checkbox" id="fillchk">fill</label>
    <label>col<input type="color" id="col" value="#ffffff"></label>
    <label>size<input type="number" id="brush" value="1" min="1"></label>
    <label>L<input type="number" id="dlayer" value="0" min="0"></label>
    <label>F<input type="number" id="dframe" value="0" min="0"></label>
  </div>
  <div id="stage"><img id="canvas" alt=""><canvas id="overlay"></canvas></div>
  <div id="drawmsg" class="desc" style="color:#6f7681;margin-top:.25rem"></div>
  <h2 style="margin-top:1rem">result</h2>
  <div id="result"></div>
</div>
<script>
let sessionId = null, rpcId = 0, tools = [], current = null;
let dims = {}, curId = "", curW = 0, curH = 0, drawTool = "pencil", drawing = false, pts = [], startPt = null;
async function rpc(method, params, notify) {
  const body = notify ? {jsonrpc:"2.0",method,params} : {jsonrpc:"2.0",id:++rpcId,method,params};
  const headers = {"Content-Type":"application/json","Accept":"application/json, text/event-stream"};
  if (sessionId) headers["Mcp-Session-Id"] = sessionId;
  const res = await fetch("/mcp", {method:"POST",headers,body:JSON.stringify(body)});
  const sid = res.headers.get("mcp-session-id"); if (sid) sessionId = sid;
  if (notify) return null;
  const ct = res.headers.get("content-type") || "";
  const text = await res.text();
  let msg;
  if (ct.includes("text/event-stream")) {
    for (const line of text.split(/\r?\n/)) {
      if (line.startsWith("data:")) { try { const o = JSON.parse(line.slice(5).trim()); if (o.result || o.error) msg = o; } catch(e){} }
    }
  } else { try { msg = JSON.parse(text); } catch(e){} }
  if (!msg) throw new Error("no MCP response");
  if (msg.error) throw new Error(msg.error.message || JSON.stringify(msg.error));
  return msg.result;
}
function schemaType(p) { let t = p.type; if (Array.isArray(t)) t = t.find(x => x !== "null") || "string"; return t; }
function renderTools(filter) {
  const box = document.getElementById("tools"); box.innerHTML = "";
  for (const t of tools) {
    if (filter && !t.name.includes(filter)) continue;
    const b = document.createElement("button");
    b.className = "tool" + (current && current.name === t.name ? " sel" : "");
    b.textContent = t.name;
    b.onclick = () => selectTool(t);
    box.appendChild(b);
  }
}
function selectTool(t) {
  current = t;
  document.getElementById("toolName").textContent = t.name;
  document.getElementById("toolDesc").textContent = t.description || "";
  const form = document.getElementById("form"); form.innerHTML = "";
  const props = (t.inputSchema && t.inputSchema.properties) || {};
  const req = (t.inputSchema && t.inputSchema.required) || [];
  for (const [name, p] of Object.entries(props)) {
    const ty = schemaType(p);
    const wrap = document.createElement("div"); wrap.className = "field";
    const isReq = req.includes(name);
    wrap.innerHTML = `<label>${name}${isReq?' <span class="req">*</span>':''} <span style="color:#6f7681">(${ty})</span></label>` +
      (p.description?`<div class="desc">${p.description.replace(/</g,'&lt;')}</div>`:'');
    let inp;
    if (ty === "boolean") { inp = document.createElement("input"); inp.type = "checkbox"; }
    else if (ty === "integer" || ty === "number") { inp = document.createElement("input"); inp.type = "number"; if (ty==="number") inp.step="any"; }
    else if (ty === "array" || ty === "object") { inp = document.createElement("textarea"); inp.placeholder = ty==="array"?'[ ... ] JSON':'{ ... } JSON'; }
    else { inp = document.createElement("input"); inp.type = "text"; }
    inp.dataset.name = name; inp.dataset.ty = ty; inp.dataset.req = isReq;
    if (name === "doc_id") { const sel = document.getElementById("docsel"); if (sel.value) inp.value = sel.value; }
    wrap.appendChild(inp); form.appendChild(wrap);
  }
  renderTools(document.getElementById("filter").value);
  document.getElementById("run").hidden = false;
}
function collectArgs() {
  const args = {};
  for (const inp of document.querySelectorAll("#form [data-name]")) {
    const { name, ty, req } = inp.dataset;
    if (ty === "boolean") { if (inp.checked) args[name] = true; continue; }
    const v = inp.value.trim();
    if (v === "") { if (req === "true") throw new Error("missing required: " + name); continue; }
    if (ty === "integer") args[name] = parseInt(v, 10);
    else if (ty === "number") args[name] = parseFloat(v);
    else if (ty === "array" || ty === "object") args[name] = JSON.parse(v);
    else args[name] = v;
  }
  return args;
}
async function runTool() {
  if (!current) return;
  const status = document.getElementById("status"); const out = document.getElementById("result");
  status.textContent = "running..."; status.className = "";
  let args;
  try { args = collectArgs(); } catch (e) { status.textContent = e.message; status.className = "err"; return; }
  try {
    const r = await rpc("tools/call", {name: current.name, arguments: args});
    out.innerHTML = "";
    for (const c of (r.content || [])) {
      if (c.type === "image" && c.data) { const im = document.createElement("img"); im.src = `data:${c.mimeType||'image/png'};base64,${c.data}`; out.appendChild(im); }
      else if (c.type === "text") { const pre = document.createElement("pre"); pre.textContent = c.text; if (r.isError) pre.className="err"; out.appendChild(pre); }
    }
    status.textContent = r.isError ? "error" : "ok"; status.className = r.isError ? "err" : "";
    await loadDocs(); if (args.doc_id) showDoc(args.doc_id);
  } catch (e) { status.textContent = e.message; status.className = "err"; }
}
function showDoc(id) {
  const sel = document.getElementById("docsel"); if (id) sel.value = id;
  const cur = sel.value; if (!cur) return;
  curId = cur;
  const d = dims[cur]; const ov = document.getElementById("overlay");
  if (d) { curW = d[0]; curH = d[1]; if (ov.width !== curW || ov.height !== curH) { ov.width = curW; ov.height = curH; } }
  document.getElementById("canvas").src = `/gallery/${encodeURIComponent(cur)}/render.png?scale=8&t=${Date.now()}`;
}
async function loadDocs() {
  let data; try { data = await (await fetch("/gallery/docs")).json(); } catch(e){ return; }
  const sel = document.getElementById("docsel"); const keep = sel.value;
  sel.innerHTML = "<option value=''>(pick a doc)</option>";
  for (const d of (data.documents||[])) { dims[d.id] = [d.w, d.h]; const o = document.createElement("option"); o.value = d.id; o.textContent = `${d.id} (${d.w}x${d.h})`; sel.appendChild(o); }
  if (keep) sel.value = keep;
}
const overlay = document.getElementById("overlay"), octx = overlay.getContext("2d");
function hex2rgba(h){ return [parseInt(h.slice(1,3),16), parseInt(h.slice(3,5),16), parseInt(h.slice(5,7),16), 255]; }
function brushSize(){ return Math.max(1, parseInt(document.getElementById("brush").value,10)||1); }
function drawColor(){ return drawTool==="eraser" ? [0,0,0,0] : hex2rgba(document.getElementById("col").value); }
function isFill(){ return document.getElementById("fillchk").checked; }
function previewCss(){ return drawTool==="eraser" ? "rgba(255,80,80,.5)" : document.getElementById("col").value; }
function docPt(e){
  const r = overlay.getBoundingClientRect();
  const x = Math.floor((e.clientX-r.left)/r.width*curW), y = Math.floor((e.clientY-r.top)/r.height*curH);
  return [Math.max(0,Math.min(curW-1,x)), Math.max(0,Math.min(curH-1,y))];
}
function clearPrev(){ octx.clearRect(0,0,overlay.width,overlay.height); }
function stampPrev(x,y){ octx.fillStyle = previewCss(); const s = brushSize(), o = (s-1)>>1; octx.fillRect(x-o,y-o,s,s); }
async function emit(name, args){
  const msg = document.getElementById("drawmsg");
  if(!curId){ msg.textContent = "pick a doc first"; return; }
  args.doc_id = curId; args.layer = parseInt(document.getElementById("dlayer").value,10)||0; args.frame = parseInt(document.getElementById("dframe").value,10)||0;
  try {
    const r = await rpc("tools/call", {name, arguments: args});
    const t = (r.content||[]).find(c => c.type==="text");
    msg.textContent = (r.isError ? "err: " : name + " ✓  ") + (t ? t.text : ""); msg.className = r.isError ? "err" : "desc";
  } catch(e){ msg.textContent = "err: " + e.message; msg.className = "err"; }
  showDoc(curId); loadDocs();
}
overlay.addEventListener("pointerdown", e => {
  if(!curId || !curW){ document.getElementById("drawmsg").textContent = "pick a doc first"; return; }
  e.preventDefault(); overlay.setPointerCapture(e.pointerId);
  const p = docPt(e);
  if(drawTool==="fill"){ emit("doc_draw", {op:"fill", x:p[0], y:p[1], color:drawColor()}); return; }
  drawing = true; startPt = p; pts = [p]; clearPrev();
  if(drawTool==="pencil" || drawTool==="eraser") stampPrev(p[0], p[1]);
});
overlay.addEventListener("pointermove", e => {
  if(!drawing) return;
  const p = docPt(e);
  if(drawTool==="pencil" || drawTool==="eraser"){
    const last = pts[pts.length-1];
    if(p[0]!==last[0] || p[1]!==last[1]){ pts.push(p); stampPrev(p[0], p[1]); }
    return;
  }
  clearPrev(); octx.strokeStyle = previewCss(); octx.fillStyle = previewCss(); octx.lineWidth = 1;
  const x0=startPt[0], y0=startPt[1], x1=p[0], y1=p[1];
  if(drawTool==="line"){ octx.beginPath(); octx.moveTo(x0+.5,y0+.5); octx.lineTo(x1+.5,y1+.5); octx.stroke(); }
  else if(drawTool==="rect"){ const x=Math.min(x0,x1), y=Math.min(y0,y1), w=Math.abs(x1-x0)+1, h=Math.abs(y1-y0)+1; isFill()?octx.fillRect(x,y,w,h):octx.strokeRect(x+.5,y+.5,Math.max(w-1,0),Math.max(h-1,0)); }
  else if(drawTool==="ellipse"){ const cx=(x0+x1)/2, cy=(y0+y1)/2, rx=Math.max(Math.abs(x1-x0)/2,.5), ry=Math.max(Math.abs(y1-y0)/2,.5); octx.beginPath(); octx.ellipse(cx+.5,cy+.5,rx,ry,0,0,7); isFill()?octx.fill():octx.stroke(); }
});
overlay.addEventListener("pointerup", e => {
  if(!drawing) return; drawing = false;
  const p = docPt(e), col = drawColor(), sz = brushSize(), fill = isFill();
  const x0=startPt[0], y0=startPt[1], x1=p[0], y1=p[1];
  clearPrev();
  if(drawTool==="pencil" || drawTool==="eraser"){ if(pts.length) emit("doc_draw", {op:"pencil", points:pts, color:col, size:sz}); }
  else if(drawTool==="line"){ emit("doc_draw", {op:"line", x0,y0,x1,y1, color:col, size:sz}); }
  else if(drawTool==="rect"){ emit("doc_draw", {op:"rect", x0,y0,x1,y1, color:col, fill, size:sz}); }
  else if(drawTool==="ellipse"){ const cx=Math.round((x0+x1)/2), cy=Math.round((y0+y1)/2), rx=Math.round(Math.abs(x1-x0)/2), ry=Math.round(Math.abs(y1-y0)/2); if(rx>0||ry>0) emit("doc_draw", {op:"ellipse", cx,cy, rx:Math.max(rx,1), ry:Math.max(ry,1), color:col, fill}); }
  pts = []; startPt = null;
});
for(const b of document.querySelectorAll("#drawbar [data-dt]")){
  b.onclick = () => { drawTool = b.dataset.dt; for(const x of document.querySelectorAll("#drawbar [data-dt]")) x.classList.toggle("sel", x===b); };
}
document.getElementById("filter").oninput = e => renderTools(e.target.value);
document.getElementById("run").onclick = runTool;
document.getElementById("docsel").onchange = () => showDoc();
(async () => {
  try {
    await rpc("initialize", {protocolVersion:"2025-06-18", capabilities:{}, clientInfo:{name:"atelier-playground",version:"1"}});
    await rpc("notifications/initialized", {}, true);
    tools = ((await rpc("tools/list", {})).tools || []).sort((a,b)=>a.name.localeCompare(b.name));
    renderTools("");
    await loadDocs();
    // Live render: the server pushes a changed doc_id on every successful tool
    // call (agent or this page) — re-render instantly. Poll stays as a fallback.
    try {
      const es = new EventSource("/gallery/events");
      es.onmessage = (e) => { try { const m = JSON.parse(e.data); if (!drawing && m.doc === curId) showDoc(curId); } catch (_) {} };
    } catch (err) {}
    setInterval(() => { if (!drawing) showDoc(); }, 2500);
  } catch (e) { document.getElementById("tools").innerHTML = `<span class="err">connect failed: ${e.message}</span>`; }
})();
</script>
</body>
</html>"##;

/// A focused, read-only LIVE view of ONE document: pick a doc, then watch the
/// canvas re-render and a feed of the tool calls hitting it stream in real time
/// (driven by the same `/gallery/events` SSE the gallery uses). For watching an
/// agent draw — no tool forms, no editing.
const LIVE_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>atelier live</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin: 0; height: 100vh; display: flex; flex-direction: column;
         background: #14161a; color: #e6e8ec; overflow: hidden;
         font: 13px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }
  header { display: flex; align-items: center; gap: .75rem; padding: .6rem .9rem;
           border-bottom: 1px solid #2a2e36; }
  header h1 { font-size: 1rem; margin: 0; }
  header .live { font-size: 11px; color: #6f7681; }
  header .dot { color: #3a8a62; }
  select { padding: .35rem; background: #0c0d10; border: 1px solid #2a2e36;
           border-radius: 5px; color: #e6e8ec; min-width: 16rem; }
  main { flex: 1; display: grid; grid-template-columns: 1fr 26rem; min-height: 0; }
  #stage { display: flex; align-items: center; justify-content: center; padding: 1rem;
           overflow: auto; background:
             repeating-conic-gradient(#1a1d22 0% 25%, #16181d 0% 50%) 50% / 24px 24px; }
  #canvas { max-width: 100%; max-height: 100%; image-rendering: pixelated;
            border: 1px solid #2a2e36; border-radius: 6px; background: #0c0d10; }
  #side { border-left: 1px solid #2a2e36; display: flex; flex-direction: column; min-height: 0; }
  #side h2 { font-size: .8rem; color: #8b919c; margin: 0; padding: .6rem .8rem;
             border-bottom: 1px solid #2a2e36; text-transform: uppercase; letter-spacing: .05em;
             display: flex; justify-content: space-between; }
  #feed { flex: 1; overflow-y: auto; padding: .4rem; }
  .row { padding: .35rem .5rem; border-radius: 5px; margin: 1px 0; background: #1a1d22;
         animation: in .25s ease; }
  @keyframes in { from { background: #2a4a38; } to { background: #1a1d22; } }
  .row .tool { color: #9ad5b0; font-weight: 600; }
  .row .args { color: #7d8590; word-break: break-all; }
  .row .t { color: #565c66; float: right; font-size: 11px; }
  #empty { color: #6f7681; padding: 1rem; }
</style>
</head>
<body>
<header>
  <h1>atelier <span class="live"><span class="dot">●</span> live</span></h1>
  <select id="docsel"></select>
  <span class="live" id="dims"></span>
</header>
<main>
  <div id="stage"><img id="canvas" alt=""></div>
  <div id="side">
    <h2><span>tool calls</span><span id="count">0</span></h2>
    <div id="feed"><div id="empty">pick a document, then watch the agent work…</div></div>
  </div>
</main>
<script>
let curId = "", dims = {}, count = 0;
function showDoc(id) {
  const sel = document.getElementById("docsel"); if (id) sel.value = id;
  curId = sel.value; if (!curId) return;
  const d = dims[curId];
  document.getElementById("dims").textContent = d ? `${curId} · ${d[0]}×${d[1]}` : curId;
  document.getElementById("canvas").src = `/gallery/${encodeURIComponent(curId)}/render.png?scale=8&t=${Date.now()}`;
}
async function loadDocs() {
  let data; try { data = await (await fetch("/gallery/docs")).json(); } catch (e) { return; }
  const sel = document.getElementById("docsel"); const keep = sel.value;
  sel.innerHTML = "<option value=''>(pick a document)</option>";
  for (const d of (data.documents || [])) { dims[d.id] = [d.w, d.h];
    const o = document.createElement("option"); o.value = d.id; o.textContent = `${d.id} (${d.w}×${d.h})`; sel.appendChild(o); }
  if (keep) sel.value = keep;
}
function logCall(m) {
  const feed = document.getElementById("feed");
  const empty = document.getElementById("empty"); if (empty) empty.remove();
  const now = new Date();
  const hh = String(now.getHours()).padStart(2,"0"), mm = String(now.getMinutes()).padStart(2,"0"), ss = String(now.getSeconds()).padStart(2,"0");
  const row = document.createElement("div"); row.className = "row";
  row.innerHTML = `<span class="t">${hh}:${mm}:${ss}</span><span class="tool"></span> <span class="args"></span>`;
  row.querySelector(".tool").textContent = m.tool || "?";
  row.querySelector(".args").textContent = m.args || "";
  feed.insertBefore(row, feed.firstChild);
  while (feed.children.length > 250) feed.removeChild(feed.lastChild);
  document.getElementById("count").textContent = ++count;
}
document.getElementById("docsel").onchange = () => {
  count = 0; document.getElementById("count").textContent = 0;
  document.getElementById("feed").innerHTML = "";
  showDoc();
};
(async () => {
  await loadDocs();
  try {
    const es = new EventSource("/gallery/events");
    es.onmessage = async (e) => {
      let m; try { m = JSON.parse(e.data); } catch (_) { return; }
      // New doc the dropdown hasn't seen (e.g. doc_create) — refresh the list so
      // it's selectable, and auto-attach if we're not already watching one.
      if (!(m.doc in dims)) await loadDocs();
      if (!curId) showDoc(m.doc);
      if (m.doc !== curId) return;
      logCall(m);
      showDoc(curId);
    };
  } catch (err) {}
  setInterval(loadDocs, 5000);
})();
</script>
</body>
</html>"##;

/// Query params for the gallery render route (`?frame=0&scale=4`).
#[derive(Deserialize)]
struct GalleryRender {
    frame: Option<usize>,
    scale: Option<u32>,
}

/// GET /gallery/docs — the live document list the page polls (same shape as the
/// `list_docs` tool). Locks the studio only to read the index.
async fn gallery_docs(
    axum::extract::State(studio): axum::extract::State<std::sync::Arc<std::sync::Mutex<Studio>>>,
) -> axum::response::Json<Value> {
    let docs = studio.lock().expect("studio lock poisoned").list_docs();
    axum::response::Json(docs)
}

/// GET /gallery/:id/render.png — one frame as PNG bytes (scale clamped 1..=16).
/// Unknown ids 404; the lock is held only around the render itself.
async fn gallery_render(
    axum::extract::State(studio): axum::extract::State<std::sync::Arc<std::sync::Mutex<Studio>>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<GalleryRender>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let bytes = {
        let studio = studio.lock().expect("studio lock poisoned");
        studio.render_png_bytes(&id, q.frame.unwrap_or(0), gallery_scale(q.scale))
    };
    match bytes {
        Ok(png) => ([(axum::http::header::CONTENT_TYPE, "image/png")], png).into_response(),
        // Generic body — don't leak the studio error that enumerates all doc ids.
        Err(_) => (axum::http::StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// GET /gallery/events — a Server-Sent Events stream of changed document ids.
/// Every successful mutating tool call broadcasts its `doc_id`; the live gallery
/// and playground subscribe and re-render that document instantly (push, not the
/// 2.5s poll). `KeepAlive` holds the connection open through idle periods.
async fn gallery_events(
    change_tx: tokio::sync::broadcast::Sender<String>,
) -> axum::response::sse::Sse<
    impl tokio_stream::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt;
    let stream = BroadcastStream::new(change_tx.subscribe())
        .filter_map(|r| r.ok().map(|id| Ok(Event::default().data(id))));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// True when `host` (a raw `Host` header) matches one of `allowed`. Each entry
/// is a bare host or `host:port`; a portless entry matches any port (mirrors
/// rmcp's host validation). An empty allowlist allows all (rmcp's convention).
fn host_allowed(host: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    let host_only = host.rsplit_once(':').map_or(host, |(h, _)| h);
    allowed.iter().any(|a| match a.rsplit_once(':') {
        Some(_) => a == host,   // host:port entry — match the full authority
        None => a == host_only, // bare host entry — match any port
    })
}

/// Reject gallery requests whose `Host` header is not in the allowlist (403),
/// closing the DNS-rebinding hole the rmcp `/mcp` route is already protected from.
async fn gallery_host_guard(
    allowed: std::sync::Arc<Vec<String>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok());
    match host {
        Some(h) if host_allowed(h, &allowed) => next.run(req).await,
        _ => (axum::http::StatusCode::FORBIDDEN, "forbidden host").into_response(),
    }
}

/// Run as a networked MCP server over Streamable HTTP at `addr`, mounted at
/// `/mcp`. One shared studio backs all sessions (writes serialised by its Mutex).
/// `allowed_hosts` extends the loopback default for LAN/remote `Host` validation
/// (DNS-rebinding guard); pass the host(s) clients will use.
pub async fn run_http(
    addr: &str,
    allowed_hosts: Vec<String>,
    record: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    // Shared studio across all HTTP sessions.
    let studio = std::sync::Arc::new(std::sync::Mutex::new(Studio::new()));
    // One recorder shared across sessions so every call lands in one recipe.
    let recorder = record.map(Recorder::new);
    // Doc-change bus: every session's Atelier sends the touched doc_id here, the
    // live `/gallery/events` SSE stream fans it out to browsers. Lagging/closed
    // receivers are dropped silently (send errors ignored).
    let (change_tx, _) = tokio::sync::broadcast::channel::<String>(256);

    let mut config = StreamableHttpServerConfig::default();
    for h in allowed_hosts {
        if !config.allowed_hosts.contains(&h) {
            config.allowed_hosts.push(h);
        }
    }
    // Snapshot the same allowlist for the gallery guard before `config` is moved.
    let allowed = std::sync::Arc::new(config.allowed_hosts.clone());

    let factory = {
        let studio = studio.clone();
        let recorder = recorder.clone();
        let change_tx = change_tx.clone();
        move || {
            let mut atelier = Atelier::with_studio(studio.clone());
            atelier.recorder = recorder.clone();
            atelier.change_tx = Some(change_tx.clone());
            Ok(atelier)
        }
    };
    let service: StreamableHttpService<Atelier, LocalSessionManager> =
        StreamableHttpService::new(factory, Default::default(), config);

    // The read-only gallery rides the same studio Arc; its handlers carry it as
    // router state and lock only around each render. It is merged outside rmcp's
    // host validation, so guard it with the same allowlist (DNS-rebind defence).
    let gallery = axum::Router::new()
        .route(
            "/gallery",
            axum::routing::get(|| async { axum::response::Html(GALLERY_HTML) }),
        )
        .route(
            "/playground",
            axum::routing::get(|| async { axum::response::Html(PLAYGROUND_HTML) }),
        )
        .route(
            "/live",
            axum::routing::get(|| async { axum::response::Html(LIVE_HTML) }),
        )
        .route("/gallery/docs", axum::routing::get(gallery_docs))
        .route(
            "/gallery/{id}/render.png",
            axum::routing::get(gallery_render),
        )
        .route(
            "/gallery/events",
            axum::routing::get({
                let change_tx = change_tx.clone();
                move || gallery_events(change_tx.clone())
            }),
        )
        .with_state(studio.clone())
        .layer(axum::middleware::from_fn(move |req, next| {
            let allowed = allowed.clone();
            async move { gallery_host_guard(allowed, req, next).await }
        }));
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .merge(gallery);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    eprintln!("atelier MCP listening on http://{local}/mcp");
    eprintln!("atelier gallery at http://{local}/gallery");
    eprintln!("atelier playground (try the tools in a window) at http://{local}/playground");
    eprintln!("atelier live (watch the agent draw one doc) at http://{local}/live");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_writes_replayable_recipe() {
        let path = std::env::temp_dir().join("atelier-rec-roundtrip.json");
        let _ = std::fs::remove_file(&path);
        let rec = Recorder::new(path.clone());

        rec.record("doc_create", json!({"name": "x", "width": 8, "height": 8}));
        rec.record("doc_info", json!({"doc_id": "x"}));

        // The file each rewrite leaves must parse through replay's own parser.
        let src = std::fs::read_to_string(&path).expect("recipe file written");
        let recipe = crate::recipe::Recipe::parse(&src).expect("recipe parses");

        // Name is the file stem; description carries the recorded marker.
        assert_eq!(recipe.name, "atelier-rec-roundtrip");
        assert!(
            recipe.description.starts_with("recorded session "),
            "got: {}",
            recipe.description
        );
        // Steps round-trip in order with their args intact.
        assert_eq!(recipe.steps.len(), 2);
        assert_eq!(recipe.steps[0].tool, "doc_create");
        assert_eq!(recipe.steps[0].args["width"], 8);
        assert_eq!(recipe.steps[1].tool, "doc_info");
        assert_eq!(recipe.steps[1].args["doc_id"], "x");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorder_skips_failed_steps() {
        let path = std::env::temp_dir().join("atelier-rec-skip.json");
        let _ = std::fs::remove_file(&path);
        let rec = Recorder::new(path.clone());

        // Mirror call_tool: record only when the result is not an error payload.
        let ok = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(j(
            json!({"id": "x"}),
        ))]);
        let err = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(j(
            json!({"error": "no such doc"}),
        ))]);
        assert!(!is_error_result(&ok));
        assert!(is_error_result(&err));
        if !is_error_result(&ok) {
            rec.record("doc_create", json!({"name": "x"}));
        }
        if !is_error_result(&err) {
            rec.record("doc_info", json!({"doc_id": "nope"}));
        }

        let src = std::fs::read_to_string(&path).expect("recipe file written");
        let recipe = crate::recipe::Recipe::parse(&src).expect("recipe parses");
        assert_eq!(recipe.steps.len(), 1);
        assert_eq!(recipe.steps[0].tool, "doc_create");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorder_creates_missing_parent_dir() {
        let base = std::env::temp_dir().join(format!("atelier-rec-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("a").join("b").join("recipe.json");
        let rec = Recorder::new(path.clone()); // must create a/b/
        rec.record("doc_create", json!({"name": "x"}));
        assert!(path.exists(), "recipe written into freshly created dirs");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resource_uri_parses_and_rejects() {
        assert_eq!(
            parse_resource_uri("atelier://doc/hero"),
            Some(ResourceTarget::Structure("hero".into()))
        );
        assert_eq!(
            parse_resource_uri("atelier://doc/hero/render"),
            Some(ResourceTarget::Render("hero".into()))
        );
        // Unknown scheme, empty id, extra segments, and bare prefixes -> None.
        assert_eq!(parse_resource_uri("file:///hero"), None);
        assert_eq!(parse_resource_uri("atelier://doc/"), None);
        assert_eq!(parse_resource_uri("atelier://doc//render"), None);
        assert_eq!(parse_resource_uri("atelier://doc/hero/extra"), None);
        assert_eq!(parse_resource_uri("atelier://doc/hero/render/x"), None);
    }

    #[test]
    fn gallery_host_guard_matches_allowlist() {
        let allowed = vec!["localhost".to_string(), "example.com:9000".to_string()];
        // Bare-host entry matches any port; full authority must match exactly.
        assert!(host_allowed("localhost", &allowed));
        assert!(host_allowed("localhost:8765", &allowed));
        assert!(host_allowed("example.com:9000", &allowed));
        assert!(!host_allowed("example.com:1", &allowed)); // wrong port
        assert!(!host_allowed("evil.example", &allowed));
        // Empty allowlist allows all (rmcp's convention).
        assert!(host_allowed("anything", &[]));
    }

    #[test]
    fn gallery_scale_clamps_and_defaults() {
        assert_eq!(gallery_scale(None), 4); // absent -> doc_look default
        assert_eq!(gallery_scale(Some(0)), 1); // 0 floors to 1
        assert_eq!(gallery_scale(Some(8)), 8); // in range passes through
        assert_eq!(gallery_scale(Some(999)), 16); // caps at 16
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors exercise the 0/1/2 trailing-byte padding cases.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// Build an Atelier whose studio is rooted at a throwaway temp dir.
    fn temp_atelier(tag: &str) -> Atelier {
        let dir = std::env::temp_dir().join(format!("atelier-srv-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let studio = Studio::with_docs_dir(dir);
        Atelier::with_studio(std::sync::Arc::new(std::sync::Mutex::new(studio)))
    }

    #[test]
    fn fetch_returns_structure_json_and_png_blob() {
        let a = temp_atelier("fetch");
        a.studio().doc_create("Hero", 8, 8).unwrap();

        // Structure resource: JSON text carrying the document's dimensions.
        let s = a
            .fetch_resource(
                "atelier://doc/hero",
                ResourceTarget::Structure("hero".into()),
            )
            .expect("structure fetched");
        match s {
            ResourceContents::TextResourceContents {
                text, mime_type, ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("application/json"));
                let v: Value = serde_json::from_str(&text).expect("valid json");
                assert_eq!(v["w"], 8);
                assert_eq!(v["id"], "hero");
            }
            other => panic!("expected text contents, got {other:?}"),
        }

        // Render resource: a base64 PNG blob (verify the magic bytes decode out).
        let r = a
            .fetch_resource(
                "atelier://doc/hero/render",
                ResourceTarget::Render("hero".into()),
            )
            .expect("render fetched");
        match r {
            ResourceContents::BlobResourceContents {
                blob, mime_type, ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("image/png"));
                // base64 of a PNG always starts with the "iVBOR" signature.
                assert!(blob.starts_with("iVBOR"), "not a PNG blob: {}", &blob[..8]);
            }
            other => panic!("expected blob contents, got {other:?}"),
        }
    }

    #[test]
    fn list_resource_specs_pairs_each_doc() {
        let a = temp_atelier("list");
        a.studio().doc_create("Hero", 8, 8).unwrap();
        let specs = a.list_resource_specs();
        let uris: Vec<&str> = specs.iter().map(|r| r.uri.as_str()).collect();
        assert!(uris.contains(&"atelier://doc/hero"));
        assert!(uris.contains(&"atelier://doc/hero/render"));
        // Both carry their advertised mime types.
        for r in &specs {
            assert!(r.mime_type.is_some());
        }
    }

    #[test]
    fn fetch_unknown_doc_errors() {
        let a = temp_atelier("missing");
        let err = a
            .fetch_resource(
                "atelier://doc/ghost",
                ResourceTarget::Structure("ghost".into()),
            )
            .unwrap_err();
        assert!(err.contains("no document"), "got: {err}");
    }

    #[test]
    fn iso_date_is_well_formed() {
        let d = iso_date();
        let parts: Vec<&str> = d.split('-').collect();
        assert_eq!(parts.len(), 3, "got: {d}");
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
        let (y, m, day): (i64, u32, u32) = (
            parts[0].parse().unwrap(),
            parts[1].parse().unwrap(),
            parts[2].parse().unwrap(),
        );
        assert!(y >= 2025, "year too small: {y}");
        assert!((1..=12).contains(&m), "month: {m}");
        assert!((1..=31).contains(&day), "day: {day}");
    }

    /// Build a `{name: value}` argument object the way a get_prompt request carries it.
    fn args(pairs: &[(&str, &str)]) -> Option<serde_json::Map<String, Value>> {
        Some(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
                .collect(),
        )
    }

    #[test]
    fn prompt_substitutes_args_and_falls_back() {
        // Provided args (incl. an optional one) are substituted verbatim.
        let (text, _) = build_prompt(
            "pixel-sprite",
            &args(&[
                ("subject", "a knight"),
                ("size", "48"),
                ("palette_hint", "cool steel"),
            ]),
        )
        .expect("known prompt");
        assert!(text.contains("a knight"), "{text}");
        assert!(text.contains("48x48"), "{text}");
        assert!(text.contains("cool steel"), "{text}");

        // Omitted optional args fall back to the template default (no `{size}` leaks).
        let (text, _) = build_prompt("pixel-sprite", &args(&[("subject", "a slime")])).expect("ok");
        assert!(text.contains("a slime"), "{text}");
        assert!(text.contains("32x32"), "{text}");
        assert!(
            !text.contains('{') && !text.contains('}'),
            "unfilled slot: {text}"
        );

        // Unknown prompt name is rejected.
        assert!(build_prompt("nope", &None).is_none());
    }

    #[test]
    fn every_prompt_references_real_tools() {
        // The live tool list (the drift target).
        let names: std::collections::HashSet<String> = Atelier::new()
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();

        for spec in PROMPTS {
            // Render with no args so we exercise every fallback branch too.
            let text = (spec.build)(&|_| None);
            for tool in spec.tools {
                // Each declared tool name must actually exist...
                assert!(
                    names.contains(*tool),
                    "prompt {} names missing tool {tool}",
                    spec.name
                );
                // ...and must appear verbatim in the rendered guidance.
                assert!(
                    text.contains(tool),
                    "prompt {} omits its declared tool {tool}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn core_profile_names_are_all_real_tools() {
        // A typo in CORE_TOOLS would silently drop a core tool from the default
        // profile — guard it against the live tool list.
        let names: std::collections::HashSet<String> = Atelier::new()
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for t in CORE_TOOLS {
            assert!(names.contains(*t), "CORE_TOOLS names missing tool {t}");
        }
        // Core is a strict, smaller subset of the full surface.
        assert!(CORE_TOOLS.len() < names.len());
    }

    #[test]
    fn prompt_specs_advertise_every_prompt() {
        let specs = prompt_specs();
        let names: Vec<&str> = specs.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"pixel-sprite"));
        assert!(names.contains(&"walk-cycle"));
        assert!(names.contains(&"seamless-tile"));
        // Each carries a description and its required `subject`/`character`/`material` arg.
        for p in &specs {
            assert!(p.description.is_some(), "{} has no description", p.name);
            let req = p
                .arguments
                .as_ref()
                .expect("args advertised")
                .iter()
                .filter(|a| a.required == Some(true))
                .count();
            assert!(req >= 1, "{} advertises no required arg", p.name);
        }
    }
}
