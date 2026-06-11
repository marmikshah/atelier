//! atelier MCP server (rmcp). Exposes the headless document editor as MCP
//! tools over stdio or Streamable HTTP. Tools return JSON strings; studio errors
//! come back as {"error": ...} payloads rather than failing the call.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, ListResourcesResult, PaginatedRequestParams,
    RawResource, ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::studio::Studio;

fn j(v: Value) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
}

/// Wraps a studio result as a JSON string: errors become {"error": ...} payloads
/// rather than failing the MCP call.
fn res(r: Result<Value, String>) -> String {
    match r {
        Ok(v) => j(v),
        Err(e) => j(json!({"error": e})),
    }
}

/// Wrap an image-producing studio result as MCP content: an inline PNG (so the
/// agent SEES the pixels in the same turn — no separate file read) plus a JSON
/// text part with the measured stats. Errors come back as a `{"error": ...}`
/// text part, matching `res`.
fn img_result(r: Result<(Vec<u8>, Value), String>) -> CallToolResult {
    match r {
        Ok((png, report)) => CallToolResult::success(vec![
            Content::image(base64(&png), "image/png"),
            Content::text(j(report)),
        ]),
        Err(e) => CallToolResult::success(vec![Content::text(j(json!({"error": e})))]),
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

/// [[x,y],...] -> Vec<(i32,i32)> for polyline/polygon vertices.
fn points(v: &[Vec<i64>]) -> Vec<(i32, i32)> {
    v.iter()
        .map(|pt| {
            (
                pt.first().copied().unwrap_or(0) as i32,
                pt.get(1).copied().unwrap_or(0) as i32,
            )
        })
        .collect()
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
pub struct DocAddLayer {
    pub doc_id: String,
    pub name: Option<String>,
    pub opacity: Option<u8>,
    pub blend: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSetLayer {
    pub doc_id: String,
    pub layer: usize,
    pub visible: Option<bool>,
    pub opacity: Option<u8>,
    pub blend: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocAddFrame {
    pub doc_id: String,
    pub duration_ms: Option<u32>,
    pub copy_from: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocFrameDuration {
    pub doc_id: String,
    pub frame: usize,
    pub duration_ms: u32,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocAddTag {
    pub doc_id: String,
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub direction: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocFillCel {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub color: Vec<i64>,
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
    /// Nearest-neighbour scale factor (default 1.0).
    pub scale: Option<f32>,
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
pub struct DocSymmetry {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Column to mirror left↔right across (omit to skip vertical mirroring).
    pub vertical: Option<i32>,
    /// Row to mirror top↔bottom across (omit to skip horizontal mirroring).
    pub horizontal: Option<i32>,
    /// For a vertical axis, reflect the left side onto the right (default true).
    pub keep_left: Option<bool>,
    /// For a horizontal axis, reflect the top onto the bottom (default true).
    pub keep_top: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRender {
    pub doc_id: String,
    pub frame: Option<usize>,
    pub out_path: Option<String>,
    pub scale: Option<u32>,
    /// Crop to [x0,y0,x1,y1] document pixels (cheap region preview).
    pub region: Option<Vec<i32>>,
    /// Ghost the previous (blue) and next (red) frames behind this one.
    pub onion: Option<bool>,
    /// Repeat the result in an N×N grid to check seamless tiling (default 1).
    pub tile: Option<u32>,
    /// Down-scale the longest side to at most this many pixels (thumbnail).
    pub max_size: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocExport {
    pub doc_id: String,
    pub out_path: String,
    pub scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocExportGif {
    pub doc_id: String,
    pub out_path: String,
    pub scale: Option<u32>,
    /// Animation tag to play (honours its direction: forward/reverse/pingpong).
    /// Omit to play the whole timeline forward.
    pub tag: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocExportTileset {
    pub doc_id: String,
    /// Tile width in source pixels; the canvas width must divide by it exactly.
    pub tile_w: u32,
    /// Tile height in source pixels; the canvas height must divide by it exactly.
    pub tile_h: u32,
    /// Nearest-neighbour upscale of the PNG and tile size (default 1).
    pub scale: Option<u32>,
    pub out_path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocWangTiles {
    pub doc_id: String,
    /// Tile size N in pixels; the source canvas must be at least N×N.
    pub n: u32,
}

// --- drawing params --------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct DocPencil {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// List of [x,y] pixels to paint.
    pub points: Vec<Vec<i64>>,
    /// [r,g,b] or [r,g,b,a]; alpha 0 erases.
    pub color: Vec<i64>,
    pub size: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocLine {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub color: Vec<i64>,
    pub size: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocRect {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub color: Vec<i64>,
    pub fill: Option<bool>,
    pub size: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocEllipse {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub cx: i32,
    pub cy: i32,
    pub rx: i32,
    pub ry: i32,
    pub color: Vec<i64>,
    pub fill: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocFill {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub color: Vec<i64>,
    pub tolerance: Option<i32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocReplace {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub from: Vec<i64>,
    pub to: Vec<i64>,
    pub tolerance: Option<i32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocFlip {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub horizontal: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocShift {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub dx: i32,
    pub dy: i32,
    /// Roll pixels around the edges (toroidal) for seamless tiles. Default false.
    pub wrap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocBlur {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub radius: i32,
    /// Optional region [x0,y0,x1,y1]; omit for the whole cel.
    pub region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocQuantize {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Target palette (each [r,g,b]/[r,g,b,a]). Empty ⇒ derive `max_colors` from
    /// the cel by median cut.
    pub colors: Option<Vec<Vec<i64>>>,
    /// Colours to derive when `colors` is empty (default 16).
    pub max_colors: Option<usize>,
}

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
pub struct DocOutline {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub color: Vec<i64>,
    /// Soften diagonal corner pixels (anti-aliased outline). Default false.
    pub aa: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocDropShadow {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Shadow offset (default 1,1).
    pub dx: Option<i32>,
    pub dy: Option<i32>,
    pub color: Option<Vec<i64>>,
    /// Shadow opacity 0..255 (default 160).
    pub opacity: Option<u8>,
    /// Blur radius in pixels (default 0).
    pub blur: Option<i32>,
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
}

#[derive(Deserialize, JsonSchema)]
pub struct DocBevel {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Top/left highlight colour (its alpha = strength). Default white ~50%.
    pub light: Option<Vec<i64>>,
    /// Bottom/right shadow colour (its alpha = strength). Default black ~50%.
    pub dark: Option<Vec<i64>>,
    /// Edge band thickness in pixels (default 1).
    pub depth: Option<i32>,
}

/// One gradient colour stop: position along the axis (0..1) + RGBA colour.
#[derive(Deserialize, JsonSchema)]
pub struct GradientStop {
    pub pos: f32,
    pub color: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocGradient {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// "linear" (axis x0,y0 -> x1,y1) or "radial" (centre x0,y0, rim at x1,y1).
    pub kind: Option<String>,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    /// Colour stops, e.g. [{"pos":0,"color":[20,30,60]},{"pos":1,"color":[180,200,255]}].
    pub stops: Vec<GradientStop>,
    /// "none" (smooth lerp), "bayer" (ordered) or "noise" (seeded) dithering.
    pub dither: Option<String>,
    /// Seed for "noise" dithering (deterministic).
    pub seed: Option<u64>,
    /// Optional clip rect [x0,y0,x1,y1] (inclusive); omit to fill the whole cel.
    pub region: Option<Vec<i32>>,
    /// true (default) composites over existing pixels so stop alpha is a real
    /// falloff (vignette/light); false overwrites.
    pub blend: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocScatter {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    /// Colours to pick from, each [r,g,b] or [r,g,b,a].
    pub colors: Vec<Vec<i64>>,
    /// Per-pixel paint probability 0..1.
    pub density: f64,
    /// Seed so the scatter reproduces exactly.
    pub seed: Option<u64>,
    /// Square dot size (default 1).
    pub size: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocShade {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Light origin: "top-left" (default) | "top" | "top-right" | "left" |
    /// "right" | "bottom-left" | "bottom" | "bottom-right".
    pub light_dir: Option<String>,
    /// How far to push lit/shadow pixels along the ramp or in lightness (def 1).
    pub steps: Option<i32>,
    /// Optional region [x0,y0,x1,y1] to confine shading; omit for the whole cel.
    pub region: Option<Vec<i32>>,
    /// "both" (default) | "highlight" | "shadow".
    pub mode: Option<String>,
    /// Optional shading ramp ordered dark→light (each [r,g,b]/[r,g,b,a]); when
    /// given, touched pixels snap to it and move ±steps. Omit for an HSL shift
    /// (warm highlights, cool shadows).
    pub ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocForm {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Form model: "sphere" (default) | "cylinder-h" | "cylinder-v" | "auto".
    /// "auto" derives volume from the silhouette's interior distance (any shape).
    pub form: Option<String>,
    /// Highlight origin — same 8 dirs as doc_shade (default "top-left").
    pub light_dir: Option<String>,
    /// Region [x0,y0,x1,y1] bounding the form; omit for the cel's opaque bbox.
    pub region: Option<Vec<i32>>,
    /// Shading ramp ordered dark→light (each [r,g,b]/[r,g,b,a]); omit to derive
    /// one from the mean fill colour.
    pub ramp: Option<Vec<Vec<i64>>>,
    /// 0..1 — compress (low) or span the full ramp (1, default).
    pub strength: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocDither {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Region [x0,y0,x1,y1] (inclusive). Required unless a selection is active
    /// (which then bounds the dither).
    pub region: Option<Vec<i32>>,
    pub color_a: Vec<i64>,
    pub color_b: Vec<i64>,
    /// "checker" | "bayer2" | "bayer4" (default) | "bayer8".
    pub pattern: Option<String>,
    /// 0..1 fraction biased toward color_b via the threshold matrix (default 0.5).
    pub density: Option<f32>,
    /// true repaints only pixels already equal to color_a or color_b (recolour
    /// a flat fill into a dither without spilling). Default false.
    pub only_existing: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPixelPerfect {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Optional region [x0,y0,x1,y1] to confine the cleanup; omit for whole cel.
    pub region: Option<Vec<i32>>,
    /// Restrict to strokes of this exact colour [r,g,b]/[r,g,b,a]; omit for any.
    pub color: Option<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPolygon {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Vertices [[x,y],...] (auto-closed).
    pub points: Vec<Vec<i64>>,
    pub color: Vec<i64>,
    /// true (default) scanline-fills the interior; false draws the outline only.
    pub fill: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPolyline {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Points [[x,y],...] joined by line segments.
    pub points: Vec<Vec<i64>>,
    pub color: Vec<i64>,
    pub size: Option<i64>,
    /// true joins the last point back to the first (closed loop).
    pub closed: Option<bool>,
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

#[derive(Deserialize, JsonSchema)]
pub struct DocAdjust {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Hue shift in degrees.
    pub hue: Option<f32>,
    /// Saturation delta -1..1.
    pub sat: Option<f32>,
    /// Lightness delta -1..1.
    pub lum: Option<f32>,
    /// Optional region [x0,y0,x1,y1]; omit for the whole cel.
    pub region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocNoise {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// "cloud" (fBm) | "perlin" | "voronoi".
    pub kind: Option<String>,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    /// Feature size in pixels (default 8).
    pub scale: Option<f32>,
    /// Octaves for "cloud" (default 4).
    pub octaves: Option<u32>,
    pub seed: Option<u64>,
    /// Colour map: stops [{pos,color},...] the noise value 0..1 indexes.
    pub stops: Vec<GradientStop>,
    /// true composites over existing pixels; false overwrites (default false).
    pub blend: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocBezier {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Control points [[x,y],...]: 2 = line, 3 = quadratic, 4+ = cubic.
    pub points: Vec<Vec<i64>>,
    pub color: Vec<i64>,
    pub size: Option<i64>,
    /// Number of sampled segments (default 24).
    pub steps: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocText {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    /// Top-left corner of the text in document pixels.
    pub x: i32,
    pub y: i32,
    /// The string to stamp. Lowercase maps to uppercase; the font covers
    /// A-Z, 0-9 and `. , : ! ? - + / ( ) '` and space. Unknown chars render as
    /// a hollow box.
    pub text: String,
    /// [r,g,b] or [r,g,b,a]; alpha 0 erases.
    pub color: Vec<i64>,
    /// Integer pixel scale of the 3×5 cell (default 1), clamped to 1..=64.
    pub size: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PaletteRamp {
    /// Base colour [r,g,b] or [r,g,b,a].
    pub base: Vec<i64>,
    /// Number of shades (darkest → lightest).
    pub count: usize,
    /// Hue shift per end in degrees (warm highlights / cool shadows). Default 20.
    pub hue_shift: Option<f32>,
    /// Half-spread in lightness 0..1 (default 0.35).
    pub light_range: Option<f32>,
    /// Saturation spread (shadows gain, highlights lose). Default 0.1.
    pub sat_shift: Option<f32>,
    /// If set, also store the ramp as this document's palette.
    pub doc_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPixel {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
}

/// A rectangular region of a cel (inclusive corners) + optional offset.
#[derive(Deserialize, JsonSchema)]
pub struct DocRegion {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocMoveRegion {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub dx: i32,
    pub dy: i32,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPaste {
    pub doc_id: String,
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    /// true = source-over (keep dest under transparent source); false = overwrite.
    pub blend: Option<bool>,
}

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
pub struct DocRenderValue {
    pub doc_id: String,
    pub frame: Option<usize>,
    /// "grayscale" (luma), "bands" (posterised luma), "saturation" or "hue".
    pub mode: String,
    /// Number of even posterise steps for mode="bands" (default 4).
    pub bands: Option<u32>,
    pub scale: Option<u32>,
    pub out_path: Option<String>,
    /// Add value stats (min/max/mean/contrast/band coverage) over opaque pixels.
    pub report: Option<bool>,
}

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
    /// "seam" (loop wrap diff) or "spacing" (per-frame motion evenness).
    pub mode: String,
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

// --- world-class-art tool params (see docs/ART-QUALITY-REVIEW.md) ----------

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
pub struct DocLayerOps {
    pub doc_id: String,
    /// move | insert | delete | rename | duplicate | merge_down.
    pub action: String,
    pub index: Option<usize>,
    pub to_index: Option<usize>,
    pub name: Option<String>,
    pub opacity: Option<u8>,
    pub blend: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocPerceptualRamp {
    /// Base colour [r,g,b(,a)].
    pub base: Vec<i64>,
    pub count: Option<usize>,
    /// Target OKLab lightness (0..1) of the darkest / lightest step.
    pub value_lo: Option<f32>,
    pub value_hi: Option<f32>,
    /// Total hue rotation (degrees) across the ramp.
    pub hue_shift: Option<f32>,
    /// flat | arc | sat-in-shadow.
    pub sat_curve: Option<String>,
    pub anchor_midtone: Option<bool>,
    /// If set, store the ramp as this document's palette.
    pub set_doc: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocSnapPalette {
    pub doc_id: String,
    pub layer: Option<usize>,
    pub frame: Option<usize>,
    /// Override palette as a list of [r,g,b(,a)]; defaults to the doc's palette.
    pub palette: Option<Vec<Vec<i64>>>,
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
    /// Notches whose both opaque legs run longer than this stay crisp.
    pub max_run: Option<i32>,
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
}

#[derive(Deserialize, JsonSchema)]
pub struct DocHarmonyPalette {
    pub base: Vec<i64>,
    /// complementary | triadic | analogous | split | tetradic | mono.
    pub scheme: Option<String>,
    pub per_ramp: Option<usize>,
    pub value_lo: Option<f32>,
    pub value_hi: Option<f32>,
    pub hue_shift: Option<f32>,
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

// --- resources -------------------------------------------------------------

/// Scale used when rendering the `render` resource (matches the doc_render default).
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
            "palette_ramp",
            "doc_set_palette",
            "doc_rect",
            "doc_ellipse",
            "doc_polygon",
            "doc_pencil",
            "doc_line",
            "doc_batch",
            "doc_render",
            "doc_shade",
            "doc_pixel_perfect",
            "doc_silhouette",
            "doc_components",
            "doc_palette_report",
            "doc_export_sheet",
        ],
        build: |g| {
            let subject = g("subject").unwrap_or_else(|| "a sprite".into());
            let size = g("size").unwrap_or_else(|| "32".into());
            let palette = g("palette_hint").unwrap_or_else(|| "your choice".into());
            format!(
                "Draw a pixel-art sprite of {subject} on a {size}x{size} canvas (palette: {palette}).\n\
                 1. doc_create a {size}x{size} document.\n\
                 2. palette_ramp a base colour into shades, then doc_set_palette to LOCK the ramp.\n\
                 3. Block the silhouette with doc_rect / doc_ellipse / doc_polygon.\n\
                 4. Paint detail with doc_pencil / doc_line, or doc_batch many ops in one call.\n\
                 5. doc_render after every burst and LOOK at the PNG before continuing.\n\
                 6. Shade with doc_shade and clean strokes with doc_pixel_perfect.\n\
                 7. Audit shape: doc_silhouette (readable bbox/fill) and doc_components (no stray specks).\n\
                 8. Audit colour: doc_palette_report (every colour in_palette, no near-dupes).\n\
                 9. Fix what the audits flag, then doc_render once more to confirm.\n\
                 10. doc_export_sheet the finished sprite to a PNG.\n\
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
            "doc_add_frame",
            "doc_pencil",
            "doc_move_region",
            "doc_render",
            "doc_frame_diff",
            "doc_anim_audit",
            "doc_add_tag",
            "doc_export_gif",
        ],
        build: |g| {
            let character = g("character").unwrap_or_else(|| "the character".into());
            let frames = g("frames").unwrap_or_else(|| "4".into());
            format!(
                "Animate a {frames}-frame walk cycle for {character}.\n\
                 1. Start from a finished standing pose on frame 0 (use the pixel-sprite flow first).\n\
                 2. The cycle is contact -> down -> passing -> up; plan those {frames} poses.\n\
                 3. doc_add_frame with copy_from the previous frame so each pose starts from the last.\n\
                 4. Repaint ONLY what changes per pose (legs, arms) with doc_pencil / doc_move_region.\n\
                 5. doc_render every frame and LOOK; use doc_render onion=true to check overlap.\n\
                 6. Verify each adjacent pair with doc_frame_diff (only the limbs should change).\n\
                 7. doc_anim_audit mode=\"spacing\" — the per-frame motion must be even, low drift.\n\
                 8. doc_add_tag the range and doc_anim_audit mode=\"seam\" so the loop wrap is clean.\n\
                 9. Fix any uneven frame, re-diff, re-audit.\n\
                 10. doc_export_gif the tagged loop.\n\
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
            "doc_fill_cel",
            "doc_noise",
            "doc_scatter",
            "doc_render",
            "doc_shift",
            "doc_seam_report",
            "doc_palette_report",
            "doc_export_sheet",
        ],
        build: |g| {
            let material = g("material").unwrap_or_else(|| "the material".into());
            let size = g("size").unwrap_or_else(|| "32".into());
            format!(
                "Paint a seamless {size}x{size} {material} tile.\n\
                 1. doc_create a {size}x{size} document and doc_set_palette to lock the colours.\n\
                 2. Fill the base with doc_fill_cel, then texture with doc_noise / doc_scatter.\n\
                 3. doc_render to LOOK at the raw tile.\n\
                 4. doc_shift wrap=true to roll the seam into the middle, then paint over the join.\n\
                 5. Use doc_shift wrap=true again to make detail variants without breaking edges.\n\
                 6. doc_seam_report MUST return zero mismatches on both axes — fix until it does.\n\
                 7. doc_render tile=2 and eyeball the 2x2 grid for any visible repeat or seam.\n\
                 8. doc_palette_report to confirm the texture stayed on-palette.\n\
                 9. Repeat 4-7 until the seam report is clean and the grid looks continuous.\n\
                 10. doc_export_sheet the tile to a PNG.\n\
                 The tile is done only when doc_seam_report is zero."
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
    async fn doc_create(&self, Parameters(p): Parameters<DocCreate>) -> String {
        res(self.studio().doc_create(&p.name, p.width, p.height))
    }

    #[tool(description = "List all documents (id, name, size, frame/layer counts).")]
    async fn list_docs(&self) -> String {
        j(self.studio().list_docs())
    }

    #[tool(description = "Get a document's structure: layers, frames, cels, tags.")]
    async fn doc_info(&self, Parameters(p): Parameters<DocRef>) -> String {
        res(self.studio().doc_info(&p.doc_id))
    }

    #[tool(description = "Delete a document and all its files.")]
    async fn delete_doc(&self, Parameters(p): Parameters<DocRef>) -> String {
        res(self.studio().delete_doc(&p.doc_id))
    }

    #[tool(
        description = "Export every document as a spritesheet PNG (+ JSON meta) into a flat target dir."
    )]
    async fn export_all(&self, Parameters(p): Parameters<ExportAll>) -> String {
        res(self
            .studio()
            .export_all(&p.target_dir, p.scale.unwrap_or(4)))
    }

    #[tool(
        description = "Pack EVERY frame of EVERY document into one atlas PNG + master JSON map (doc/frame/rect/duration/pivot) for slicing a whole game's sprites from a single texture. max_width wraps the shelf packer."
    )]
    async fn export_atlas(&self, Parameters(p): Parameters<ExportAtlas>) -> String {
        res(self.studio().export_atlas(
            &p.out_path,
            p.scale.unwrap_or(4),
            p.max_width.unwrap_or(512),
        ))
    }

    // -- documents: editable layered/timeline sprites (Aseprite-style) --
    #[tool(
        description = "Add a layer to a document (top of the stack). opacity 0..255. blend: normal/multiply/screen/add/overlay/soft-light/hard-light/darken/lighten/color-dodge/color-burn/difference/subtract/exclusion (multiply=shadow/AO, add|screen=light/glow/bloom, overlay|soft-light=colour grade)."
    )]
    async fn doc_add_layer(&self, Parameters(p): Parameters<DocAddLayer>) -> String {
        res(self.studio().doc_add_layer(
            &p.doc_id,
            p.name,
            p.opacity.unwrap_or(255),
            p.blend.unwrap_or_else(|| "normal".into()),
        ))
    }

    #[tool(
        description = "Set a layer's visibility / opacity / blend (normal/multiply/screen/add/overlay/soft-light/hard-light/darken/lighten/color-dodge/color-burn/difference/subtract/exclusion)."
    )]
    async fn doc_set_layer(&self, Parameters(p): Parameters<DocSetLayer>) -> String {
        res(self
            .studio()
            .doc_set_layer(&p.doc_id, p.layer, p.visible, p.opacity, p.blend))
    }

    #[tool(
        description = "Append a frame to the timeline. duration_ms default 100; copy_from duplicates that frame's cels."
    )]
    async fn doc_add_frame(&self, Parameters(p): Parameters<DocAddFrame>) -> String {
        res(self
            .studio()
            .doc_add_frame(&p.doc_id, p.duration_ms.unwrap_or(100), p.copy_from))
    }

    #[tool(description = "Set a frame's duration in milliseconds.")]
    async fn doc_set_frame_duration(&self, Parameters(p): Parameters<DocFrameDuration>) -> String {
        res(self
            .studio()
            .doc_set_frame_duration(&p.doc_id, p.frame, p.duration_ms))
    }

    #[tool(
        description = "Add an animation tag (named frame range). direction: forward/reverse/pingpong."
    )]
    async fn doc_add_tag(&self, Parameters(p): Parameters<DocAddTag>) -> String {
        res(self.studio().doc_add_tag(
            &p.doc_id,
            &p.name,
            p.from,
            p.to,
            p.direction.as_deref().unwrap_or("forward"),
        ))
    }

    #[tool(description = "Fill a layer×frame cel with a solid colour [r,g,b] or [r,g,b,a].")]
    async fn doc_fill_cel(&self, Parameters(p): Parameters<DocFillCel>) -> String {
        res(self
            .studio()
            .doc_fill_cel(&p.doc_id, p.layer, p.frame, rgba(&p.color)))
    }

    #[tool(description = "Clear (empty) a layer×frame cel.")]
    async fn doc_clear_cel(&self, Parameters(p): Parameters<DocCel>) -> String {
        res(self.studio().doc_clear_cel(&p.doc_id, p.layer, p.frame))
    }

    #[tool(
        description = "Place an external PNG into a cel at (x,y) — import bridge for AI-gen/real/Figma art. Optional `scale` (nearest-neighbour) and `rotate` (degrees). By default draws OVER existing content with `opacity`+`blend` (sub-sprite reuse, no layer-per-element); `replace`=true overwrites the whole cel. Honours an active selection."
    )]
    async fn doc_stamp_image(&self, Parameters(p): Parameters<DocStampImage>) -> String {
        res(self.studio().doc_stamp_image(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x.unwrap_or(0),
            p.y.unwrap_or(0),
            &p.png_path,
            p.scale.unwrap_or(1.0),
            p.rotate.unwrap_or(0.0),
            p.opacity.unwrap_or(255),
            p.blend.as_deref().unwrap_or("normal"),
            p.replace.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Mirror a cel for instant symmetry: `vertical` (a column) reflects left↔right, `horizontal` (a row) reflects top↔bottom, both gives 4-way symmetry. keep_left/keep_top pick the source side. Draw half a sprite, mirror the rest."
    )]
    async fn doc_symmetry(&self, Parameters(p): Parameters<DocSymmetry>) -> String {
        res(self.studio().doc_symmetry(
            &p.doc_id,
            p.layer,
            p.frame,
            p.vertical,
            p.horizontal,
            p.keep_left.unwrap_or(true),
            p.keep_top.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Flatten a frame (visible layers) to a PNG preview so you can SEE the canvas. Returns the path. Options: `region` [x0,y0,x1,y1] crops, `onion` ghosts neighbour frames, `tile` repeats N×N to check seamlessness, `max_size` makes a cheap thumbnail."
    )]
    async fn doc_render(&self, Parameters(p): Parameters<DocRender>) -> String {
        res(self.studio().doc_render(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.out_path.as_deref(),
            p.scale.unwrap_or(4),
            region(&p.region),
            p.onion.unwrap_or(false),
            p.tile.unwrap_or(1),
            p.max_size,
        ))
    }

    #[tool(
        description = "Export the document as a horizontal spritesheet PNG + JSON meta (frames, durations, tags)."
    )]
    async fn doc_export_sheet(&self, Parameters(p): Parameters<DocExport>) -> String {
        res(self
            .studio()
            .doc_export_sheet(&p.doc_id, &p.out_path, p.scale.unwrap_or(4)))
    }

    #[tool(
        description = "Export the document as an animated GIF honouring per-frame durations. Pass `tag` to play one animation tag in its direction (forward/reverse/pingpong); omit to play the whole timeline forward."
    )]
    async fn doc_export_gif(&self, Parameters(p): Parameters<DocExportGif>) -> String {
        res(self.studio().doc_export_gif(
            &p.doc_id,
            &p.out_path,
            p.scale.unwrap_or(4),
            p.tag.as_deref(),
        ))
    }

    #[tool(
        description = "Export the document as an animated PNG (APNG) — lossless, full alpha (unlike GIF's 256 colours and 1-bit alpha). Pass `tag` to play one animation tag in its direction (forward/reverse/pingpong); omit to play the whole timeline forward."
    )]
    async fn doc_export_apng(&self, Parameters(p): Parameters<DocExportGif>) -> String {
        res(self.studio().doc_export_apng(
            &p.doc_id,
            &p.out_path,
            p.scale.unwrap_or(4),
            p.tag.as_deref(),
        ))
    }

    #[tool(
        description = "Slice frame 0 into a tile_w×tile_h grid and write an engine-ready tileset: the PNG plus TWO sidecars — <name>.tsx (Tiled XML: tilewidth/tileheight/tilecount/columns/image) and <name>.json (same fields). The canvas must divide exactly by the tile size. Nearest-neighbour `scale` (default 1) upscales the PNG and tile size. Returns {path,tsx,json,tilecount,columns,rows}."
    )]
    async fn doc_export_tileset(&self, Parameters(p): Parameters<DocExportTileset>) -> String {
        res(self.studio().export_tileset(
            &p.doc_id,
            p.tile_w,
            p.tile_h,
            p.scale.unwrap_or(1),
            &p.out_path,
        ))
    }

    #[tool(
        description = "Generate the deterministic 16-tile Wang/blob terrain set from a source doc: frame 0's layer 0 holds the INNER material, layer 1 the OUTER (top-left N×N of each is sampled). Creates a NEW document <id>-wang (canvas 4N×4N) holding all 16 corner combinations in a 4×4 grid (tile index = NE,SE,SW,NW corner bits); each set bit fills a quarter-disc (radius N/2) at that corner, adjacent set corners connect along their shared edge. Returns the new doc's structure + id."
    )]
    async fn doc_wang_tiles(&self, Parameters(p): Parameters<DocWangTiles>) -> String {
        res(self.studio().wang_tiles(&p.doc_id, p.n))
    }

    // -- per-pixel drawing on a cel (the editor; coords = document pixels) --
    #[tool(
        description = "Paint pixels into a cel. points=[[x,y],...], color [r,g,b]/[r,g,b,a] (alpha 0 erases), size = square brush."
    )]
    async fn doc_pencil(&self, Parameters(p): Parameters<DocPencil>) -> String {
        let pts = points(&p.points);
        res(self.studio().doc_pencil(
            &p.doc_id,
            p.layer,
            p.frame,
            pts,
            rgba(&p.color),
            p.size.unwrap_or(1) as i32,
        ))
    }

    #[tool(description = "Draw a line between (x0,y0) and (x1,y1) with a square brush.")]
    async fn doc_line(&self, Parameters(p): Parameters<DocLine>) -> String {
        res(self.studio().doc_line(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x0,
            p.y0,
            p.x1,
            p.y1,
            rgba(&p.color),
            p.size.unwrap_or(1) as i32,
        ))
    }

    #[tool(description = "Draw a rectangle (outline or filled) from (x0,y0) to (x1,y1).")]
    async fn doc_rect(&self, Parameters(p): Parameters<DocRect>) -> String {
        res(self.studio().doc_rect(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x0,
            p.y0,
            p.x1,
            p.y1,
            rgba(&p.color),
            p.fill.unwrap_or(false),
            p.size.unwrap_or(1) as i32,
        ))
    }

    #[tool(
        description = "Draw an ellipse centred at (cx,cy) with radii (rx,ry), outline or filled."
    )]
    async fn doc_ellipse(&self, Parameters(p): Parameters<DocEllipse>) -> String {
        res(self.studio().doc_ellipse(
            &p.doc_id,
            p.layer,
            p.frame,
            p.cx,
            p.cy,
            p.rx,
            p.ry,
            rgba(&p.color),
            p.fill.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Flood (bucket) fill from (x,y) with a colour. tolerance = max channel distance to spread."
    )]
    async fn doc_fill(&self, Parameters(p): Parameters<DocFill>) -> String {
        res(self.studio().doc_fill(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x,
            p.y,
            rgba(&p.color),
            p.tolerance.unwrap_or(0),
        ))
    }

    #[tool(description = "Replace every pixel near `from` with `to` across the cel (recolour).")]
    async fn doc_replace_color(&self, Parameters(p): Parameters<DocReplace>) -> String {
        res(self.studio().doc_replace_color(
            &p.doc_id,
            p.layer,
            p.frame,
            rgba(&p.from),
            rgba(&p.to),
            p.tolerance.unwrap_or(0),
        ))
    }

    #[tool(description = "Flip a cel horizontally (default) or vertically.")]
    async fn doc_flip(&self, Parameters(p): Parameters<DocFlip>) -> String {
        res(self
            .studio()
            .doc_flip(&p.doc_id, p.layer, p.frame, p.horizontal.unwrap_or(true)))
    }

    #[tool(
        description = "Shift a cel's contents by (dx,dy) pixels; exposed edges become transparent, or `wrap`=true rolls them around (toroidal) for making/checking seamless tiles."
    )]
    async fn doc_shift(&self, Parameters(p): Parameters<DocShift>) -> String {
        res(self.studio().doc_shift(
            &p.doc_id,
            p.layer,
            p.frame,
            p.dx,
            p.dy,
            p.wrap.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Box-blur a cel by `radius` (premultiplied — no dark haloes), optionally limited to `region`. Soft shadows, depth-of-field, smoke. Honours an active selection."
    )]
    async fn doc_blur(&self, Parameters(p): Parameters<DocBlur>) -> String {
        res(self
            .studio()
            .doc_blur(&p.doc_id, p.layer, p.frame, p.radius, region(&p.region)))
    }

    #[tool(
        description = "Snap every opaque pixel to the nearest colour in `colors`; with no `colors`, derive a `max_colors` palette from the cel by median cut. Returns the palette used. Posterise / down-palette imported or AI-gen art."
    )]
    async fn doc_quantize(&self, Parameters(p): Parameters<DocQuantize>) -> String {
        let palette: Vec<[u8; 4]> = p
            .colors
            .as_ref()
            .map(|cs| cs.iter().map(|c| rgba(c)).collect())
            .unwrap_or_default();
        res(self.studio().doc_quantize(
            &p.doc_id,
            p.layer,
            p.frame,
            palette,
            p.max_colors.unwrap_or(16),
        ))
    }

    #[tool(
        description = "Insert `steps` cross-faded (dissolve) in-between frames after frame `from`, interpolating every layer toward frame `to`. Reindexes later cels. Smooth a two-pose animation."
    )]
    async fn doc_tween(&self, Parameters(p): Parameters<DocTween>) -> String {
        res(self.studio().doc_tween(
            &p.doc_id,
            p.from,
            p.to,
            p.steps.unwrap_or(1),
            p.duration_ms.unwrap_or(100),
        ))
    }

    #[tool(
        description = "Draw a 1px outline around the opaque pixels of a cel. `aa`=true softens diagonal corners (anti-aliased)."
    )]
    async fn doc_outline(&self, Parameters(p): Parameters<DocOutline>) -> String {
        res(self.studio().doc_outline(
            &p.doc_id,
            p.layer,
            p.frame,
            rgba(&p.color),
            p.aa.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Drop a coloured shadow of the cel's silhouette offset by (dx,dy), at `opacity`, optionally `blur`red, with the art composited back on top. Self-contained on one cel. Honours an active selection."
    )]
    async fn doc_drop_shadow(&self, Parameters(p): Parameters<DocDropShadow>) -> String {
        let color = p.color.as_ref().map(|c| rgba(c)).unwrap_or([0, 0, 0, 255]);
        res(self.studio().doc_drop_shadow(
            &p.doc_id,
            p.layer,
            p.frame,
            p.dx.unwrap_or(1),
            p.dy.unwrap_or(1),
            color,
            p.opacity.unwrap_or(160),
            p.blur.unwrap_or(0),
        ))
    }

    #[tool(
        description = "Bloom/glow: blur a bright copy of the cel and composite it back through a light blend (`mode` screen/add) at `intensity`. `color` tints the glow (omit = the art's own colours). Honours an active selection."
    )]
    async fn doc_glow(&self, Parameters(p): Parameters<DocGlow>) -> String {
        let color = p.color.as_ref().map(|c| rgba(c));
        res(self.studio().doc_glow(
            &p.doc_id,
            p.layer,
            p.frame,
            color,
            p.radius.unwrap_or(2),
            p.intensity.unwrap_or(180),
            p.mode.as_deref().unwrap_or("screen"),
        ))
    }

    #[tool(
        description = "Fake-3D bevel: lighten the top/left edge band and darken the bottom/right band of the opaque shape (within `depth` px of an edge) for raised volume. `light`/`dark` alpha = strength. Honours an active selection."
    )]
    async fn doc_bevel(&self, Parameters(p): Parameters<DocBevel>) -> String {
        let light = p
            .light
            .as_ref()
            .map(|c| rgba(c))
            .unwrap_or([255, 255, 255, 128]);
        let dark = p.dark.as_ref().map(|c| rgba(c)).unwrap_or([0, 0, 0, 128]);
        res(self.studio().doc_bevel(
            &p.doc_id,
            p.layer,
            p.frame,
            light,
            dark,
            p.depth.unwrap_or(1),
        ))
    }

    #[tool(
        description = "Shift hue/saturation/lightness of opaque pixels (optionally only within `region`). Tint, recolour or brighten part of a cel. Honours an active selection."
    )]
    async fn doc_adjust(&self, Parameters(p): Parameters<DocAdjust>) -> String {
        res(self.studio().doc_adjust(
            &p.doc_id,
            p.layer,
            p.frame,
            region(&p.region),
            p.hue.unwrap_or(0.0),
            p.sat.unwrap_or(0.0),
            p.lum.unwrap_or(0.0),
        ))
    }

    #[tool(
        description = "Fill a region with procedural noise (kind 'cloud' fBm / 'perlin' / 'voronoi') mapped through colour `stops`. `scale` = feature size in px. Terrain, clouds, organic texture, mottling. Honours an active selection."
    )]
    async fn doc_noise(&self, Parameters(p): Parameters<DocNoise>) -> String {
        let stops: Vec<(f32, [u8; 4])> = p.stops.iter().map(|s| (s.pos, rgba(&s.color))).collect();
        res(self.studio().doc_noise(
            &p.doc_id,
            p.layer,
            p.frame,
            p.kind.as_deref().unwrap_or("cloud"),
            p.x0,
            p.y0,
            p.x1,
            p.y1,
            p.scale.unwrap_or(8.0),
            p.octaves.unwrap_or(4),
            p.seed.unwrap_or(0),
            stops,
            p.blend.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Draw a Bézier curve through control `points` (2=line, 3=quadratic, 4+=cubic) with brush `size`. Smooth organic strokes — tails, vines, hair. Honours an active selection."
    )]
    async fn doc_bezier(&self, Parameters(p): Parameters<DocBezier>) -> String {
        let pts = points(&p.points);
        res(self.studio().doc_bezier(
            &p.doc_id,
            p.layer,
            p.frame,
            pts,
            rgba(&p.color),
            p.size.unwrap_or(1) as i32,
            p.steps.unwrap_or(24) as i32,
        ))
    }

    #[tool(
        description = "Stamp `text` with the built-in 3×5 pixel font, top-left at (x,y), at integer pixel `size` (default 1). Covers A-Z, 0-9 and . , : ! ? - + / ( ) ' and space; lowercase maps to uppercase, unknown chars render as a hollow box. Returns the rendered `width` so you can lay out the next element. HUD mockups, damage numbers, lettering. Honours an active selection."
    )]
    async fn doc_text(&self, Parameters(p): Parameters<DocText>) -> String {
        res(self.studio().doc_text(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x,
            p.y,
            &p.text,
            rgba(&p.color),
            p.size.unwrap_or(1) as i32,
        ))
    }

    #[tool(
        description = "Generate a hue-shifted shading ramp from a base colour (darkest→lightest): warm highlights, cool shadows, the classic pixel-art ramp. Returns the colours (+ hex); pass `doc_id` to also store it as that document's palette."
    )]
    async fn palette_ramp(&self, Parameters(p): Parameters<PaletteRamp>) -> String {
        res(self.studio().palette_ramp(
            rgba(&p.base),
            p.count,
            p.hue_shift.unwrap_or(20.0),
            p.light_range.unwrap_or(0.35),
            p.sat_shift.unwrap_or(0.1),
            p.doc_id.as_deref(),
        ))
    }

    #[tool(
        description = "Paint a linear/radial colour gradient over a cel from colour `stops` (each {pos 0..1, color}). `dither` 'bayer'/'noise' gives band-free pixel-art skies/water/falloff; 'none' lerps. `region` [x0,y0,x1,y1] clips. `blend` true (default) composites so stop alpha is a real falloff (vignettes, light) — replaces hand-placed dither."
    )]
    async fn doc_gradient(&self, Parameters(p): Parameters<DocGradient>) -> String {
        let stops: Vec<(f32, [u8; 4])> = p.stops.iter().map(|s| (s.pos, rgba(&s.color))).collect();
        res(self.studio().doc_gradient(
            &p.doc_id,
            p.layer,
            p.frame,
            p.kind.as_deref().unwrap_or("linear"),
            p.x0,
            p.y0,
            p.x1,
            p.y1,
            stops,
            p.dither.as_deref().unwrap_or("none"),
            p.seed.unwrap_or(0),
            region(&p.region),
            p.blend.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Scatter pixels of random `colors` across a region at `density` (0..1 per-pixel chance), deterministic per `seed`. Organic grass/foliage/dust/stars/noise without hand-listing every speckle. `size` = square dot; source-over so alpha colours layer onto existing art."
    )]
    async fn doc_scatter(&self, Parameters(p): Parameters<DocScatter>) -> String {
        let colors: Vec<[u8; 4]> = p.colors.iter().map(|c| rgba(c)).collect();
        res(self.studio().doc_scatter(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x0,
            p.y0,
            p.x1,
            p.y1,
            colors,
            p.density as f32,
            p.seed.unwrap_or(0),
            p.size.unwrap_or(1) as i32,
        ))
    }

    #[tool(
        description = "Edge-lit shading: lit rims toward `light_dir`, core shadow away from it, pushed `steps` along a ramp (or HSL-shifted: warm highlights, cool shadows). `mode` 'highlight'/'shadow' limits which side. Pass `ramp` (dark→light) to keep shading palette-true. One-call form/volume on a flat silhouette. `region` clips; honours an active selection."
    )]
    async fn doc_shade(&self, Parameters(p): Parameters<DocShade>) -> String {
        let ramp = p
            .ramp
            .as_ref()
            .map(|r| r.iter().map(|c| rgba(c)).collect::<Vec<_>>());
        res(self.studio().doc_shade(
            &p.doc_id,
            p.layer,
            p.frame,
            p.light_dir.as_deref().unwrap_or("top-left"),
            p.steps.unwrap_or(1),
            region(&p.region),
            p.mode.as_deref().unwrap_or("both"),
            ramp,
        ))
    }

    #[tool(
        description = "Volume shading: lay a rounded-form light gradient across a shape's *interior* and snap it to a dark→light `ramp` — a flat-filled blob gains real volume in one call (where doc_shade only lights rims). `form` sphere (default) / cylinder-h / cylinder-v / auto; 'auto' uses the silhouette's interior-distance so any shape works, not just an ellipse. `light_dir` places the highlight. `region` bounds it (else the opaque bbox); `ramp` omitted is derived from the mean fill colour. `strength` 0..1 spans the ramp. Honours an active selection."
    )]
    async fn doc_form(&self, Parameters(p): Parameters<DocForm>) -> String {
        let ramp = p
            .ramp
            .as_ref()
            .map(|r| r.iter().map(|c| rgba(c)).collect::<Vec<_>>());
        res(self.studio().doc_form(
            &p.doc_id,
            p.layer,
            p.frame,
            p.light_dir.as_deref().unwrap_or("top-left"),
            p.form.as_deref().unwrap_or("sphere"),
            region(&p.region),
            ramp,
            p.strength.unwrap_or(1.0),
        ))
    }

    #[tool(
        description = "Fill a `region` with an ordered dither of `color_a`/`color_b` (`pattern` checker/bayer2/bayer4/bayer8). `density` 0..1 biases toward color_b. `only_existing`=true repaints just pixels already color_a/color_b (turn a flat fill into a gradient-dither without spilling). The pixel-art way to fake a mid-tone between two ramp colours. `region` required unless a selection is active."
    )]
    async fn doc_dither(&self, Parameters(p): Parameters<DocDither>) -> String {
        res(self.studio().doc_dither(
            &p.doc_id,
            p.layer,
            p.frame,
            region(&p.region),
            rgba(&p.color_a),
            rgba(&p.color_b),
            p.pattern.as_deref().unwrap_or("bayer4"),
            p.density.unwrap_or(0.5),
            p.only_existing.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Pixel-perfect cleanup: erase L-corner doubles from 1px strokes (the extra pixel that thickens an elbow), iterating to a fixpoint. `color` restricts to that exact stroke colour; `region` clips. Returns `removed`. Clean up jagged hand-drawn or line-tool strokes. Honours an active selection."
    )]
    async fn doc_pixel_perfect(&self, Parameters(p): Parameters<DocPixelPerfect>) -> String {
        let color = p.color.as_ref().map(|c| rgba(c));
        res(self
            .studio()
            .doc_pixel_perfect(&p.doc_id, p.layer, p.frame, region(&p.region), color))
    }

    #[tool(
        description = "Draw a polygon through `points` ([[x,y],...], auto-closed). fill=true (default) scanline-fills the interior; false draws the outline. Clean organic shapes — canopies, ponds, bodies."
    )]
    async fn doc_polygon(&self, Parameters(p): Parameters<DocPolygon>) -> String {
        let pts = points(&p.points);
        res(self.studio().doc_polygon(
            &p.doc_id,
            p.layer,
            p.frame,
            pts,
            rgba(&p.color),
            p.fill.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Draw connected line segments through `points` ([[x,y],...]). `closed`=true joins the last point back to the first. `size` = square brush. Open organic curves / paths."
    )]
    async fn doc_polyline(&self, Parameters(p): Parameters<DocPolyline>) -> String {
        let pts = points(&p.points);
        res(self.studio().doc_polyline(
            &p.doc_id,
            p.layer,
            p.frame,
            pts,
            rgba(&p.color),
            p.size.unwrap_or(1) as i32,
            p.closed.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Set/modify the active pixel selection so subsequent painting ops (fill/gradient/scatter/rect/ellipse/polygon/pencil/line/batch) are confined to it. shape: rect (x0,y0,x1,y1) | ellipse (cx,cy,rx,ry) | color (layer,frame + `color` or sample x,y + tolerance) | all | none (clear). mode: replace (default) | add | subtract | intersect."
    )]
    async fn doc_select(&self, Parameters(p): Parameters<DocSelect>) -> String {
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
            Some(crate::studio::ColorSelect {
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
    #[tool(
        description = "Read one pixel from a cel. Returns its RGBA array and #rrggbbaa hex (use to verify colours while editing blind)."
    )]
    async fn doc_get_pixel(&self, Parameters(p): Parameters<DocPixel>) -> String {
        res(self
            .studio()
            .doc_get_pixel(&p.doc_id, p.layer, p.frame, p.x, p.y))
    }

    // -- canvas readers (read-only analysis to SEE the canvas as data) --
    #[tool(
        description = "Dump a region of a frame as a text grid so you can read exact pixels blind. mode=\"symbol\" maps each distinct colour to a glyph (A..Z a..z 0..9) with a legend, `.`=transparent; mode=\"hex\" emits #rrggbb(aa)/`.` tokens. `layer` dumps one cel (omit = flattened). `region` [x0,y0,x1,y1] caps at 4096 px — crop large canvases."
    )]
    async fn doc_dump_region(&self, Parameters(p): Parameters<DocDumpRegion>) -> String {
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
    async fn doc_silhouette(&self, Parameters(p): Parameters<DocSilhouette>) -> String {
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
    async fn doc_components(&self, Parameters(p): Parameters<DocComponents>) -> String {
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
        description = "Coarse coverage/composition heatmap: split the flattened frame into rows×cols cells (default 8×8), each reporting opaque fill 0..1 and mean luma (null if empty), plus the content bbox and its centre offset from the canvas centre. Check balance/placement/negative space."
    )]
    async fn doc_coverage_map(&self, Parameters(p): Parameters<DocCoverageMap>) -> String {
        res(self.studio().doc_coverage_map(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.cols.unwrap_or(8),
            p.rows.unwrap_or(8),
        ))
    }

    // -- value & colour feedback (read-only analysis to judge values/colour) --
    #[tool(
        description = "Render a frame in analysis space to a PNG you can SEE: mode=\"grayscale\" (luma/value), \"bands\" (posterise luma into `bands` even steps to read the value structure), \"saturation\" or \"hue\" (that HSL channel as grey). Same output as doc_render. Pass report=true for value stats (min/max/mean grey, contrast=(max-min)/255, per-band coverage) over opaque pixels."
    )]
    async fn doc_render_value(&self, Parameters(p): Parameters<DocRenderValue>) -> String {
        res(self.studio().doc_render_value(
            &p.doc_id,
            p.frame.unwrap_or(0),
            &p.mode,
            p.bands.unwrap_or(4),
            p.scale.unwrap_or(4),
            p.out_path.as_deref(),
            p.report.unwrap_or(false),
        ))
    }

    #[tool(
        description = "WCAG contrast check. mode=\"region\" compares the mean colour inside `region` [x0,y0,x1,y1] against its 4px surround → {ratio, pass}. mode=\"palette\" scores every pair of the frame's distinct opaque colours (capped 16 — quantize first if more) and lists failures. mode=\"one-bit\" thresholds luma to a pure B/W PNG (returns path + black/white %). pass = ratio ≥ min_ratio (default 1.5)."
    )]
    async fn doc_contrast_check(&self, Parameters(p): Parameters<DocContrastCheck>) -> String {
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
    async fn doc_palette_report(&self, Parameters(p): Parameters<DocPaletteReport>) -> String {
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
    async fn doc_ramp_validate(&self, Parameters(p): Parameters<DocRampValidate>) -> String {
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
        description = "Diff two frames pixel-by-pixel: returns changed/added/removed/recolored counts and the change_bbox. `layer` diffs one cel (omit = flattened). `region` [x0,y0,x1,y1] restricts the area. grid=true adds a text map (`.`unchanged `+`added `-`removed `~`recolored, area capped 4096 px). render=\"overlay\" writes a PNG of frame_b dimmed 40% with changed pixels flagged (green=added/red=removed/yellow=recoloured). Inspect what actually moved between animation frames."
    )]
    async fn doc_frame_diff(&self, Parameters(p): Parameters<DocFrameDiff>) -> String {
        res(self.studio().doc_frame_diff(
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
        description = "Tiling seam check: wrap-test a frame's far edge against the near edge it abuts when repeated. axis=\"horizontal\" tests left↔right, \"vertical\" top↔bottom, \"both\" runs each. Per axis returns {mismatches, max_delta, worst:[[x,y,delta] ≤10]}. `threshold` is the max per-channel delta still counted a match (default 0). `out_path` renders the frame with mismatched edge pixels highlighted red. Verify seamless tiles."
    )]
    async fn doc_seam_report(&self, Parameters(p): Parameters<DocSeamReport>) -> String {
        res(self.studio().doc_seam_report(
            &p.doc_id,
            p.layer,
            p.frame.unwrap_or(0),
            p.axis.as_deref().unwrap_or("both"),
            p.threshold.unwrap_or(0),
            p.out_path.as_deref(),
        ))
    }

    #[tool(
        description = "Audit an animation loop. mode=\"seam\" diffs the wrap the loop actually plays (last→first forward, first→last reverse; pingpong has no seam → score 0 + note) and returns seam_score = changed/opaque pixels — high means a jarring loop cut. mode=\"spacing\" tracks the silhouette centre per played frame and returns per_frame_center/per_frame_offset, total_drift and evenness (stddev of step size / mean; 0 = mechanically even) — catch uneven/stuttering motion. `tag` audits one tag (omit = whole timeline)."
    )]
    async fn doc_anim_audit(&self, Parameters(p): Parameters<DocAnimAudit>) -> String {
        res(self
            .studio()
            .doc_anim_audit(&p.doc_id, p.tag.as_deref(), p.layer, &p.mode))
    }

    #[tool(
        description = "Eased multi-frame region motion. Reads the `region` [x0,y0,x1,y1] content from `from_frame` and stamps it (source-over) into every frame in (from_frame, to_frame] at an eased fraction of the total (dx,dy); to_frame gets the full offset. easing: linear/ease-in/ease-out/ease-in-out (cubic), bounce, overshoot (shoots past then settles), elastic (decaying oscillation). clear_source=true (default) clears the original rect in each destination frame so a moving limb leaves no stale copy. Frames must already exist (else error — doc_add_frame first). Returns frames_touched + per-frame offsets."
    )]
    async fn doc_keyframe_move(&self, Parameters(p): Parameters<DocKeyframeMove>) -> String {
        if p.region.len() < 4 {
            return j(json!({"error": "region must be [x0,y0,x1,y1]"}));
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

    #[tool(
        description = "Move a rectangular region of a cel by (dx,dy): copies it, clears the source, stamps it at the offset. Key tool for limb/keyframe animation."
    )]
    async fn doc_move_region(&self, Parameters(p): Parameters<DocMoveRegion>) -> String {
        res(self.studio().doc_move_region(
            &p.doc_id, p.layer, p.frame, p.x0, p.y0, p.x1, p.y1, p.dx, p.dy,
        ))
    }

    #[tool(description = "Erase a rectangular region of a cel (set transparent).")]
    async fn doc_clear_region(&self, Parameters(p): Parameters<DocRegion>) -> String {
        res(self
            .studio()
            .doc_clear_region(&p.doc_id, p.layer, p.frame, p.x0, p.y0, p.x1, p.y1))
    }

    #[tool(
        description = "Copy a rectangular region into the shared clipboard (does not modify the document). Paste with doc_paste — works across frames and documents."
    )]
    async fn doc_copy_region(&self, Parameters(p): Parameters<DocRegion>) -> String {
        res(self
            .studio()
            .doc_copy_region(&p.doc_id, p.layer, p.frame, p.x0, p.y0, p.x1, p.y1))
    }

    #[tool(description = "Cut a rectangular region: copy to clipboard, then clear the source.")]
    async fn doc_cut_region(&self, Parameters(p): Parameters<DocRegion>) -> String {
        res(self
            .studio()
            .doc_cut_region(&p.doc_id, p.layer, p.frame, p.x0, p.y0, p.x1, p.y1))
    }

    #[tool(
        description = "Paste the clipboard onto a cel at (x,y). blend=true keeps the destination under transparent source pixels; blend=false overwrites. Reuses art across frames/documents."
    )]
    async fn doc_paste(&self, Parameters(p): Parameters<DocPaste>) -> String {
        res(self.studio().doc_paste(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x,
            p.y,
            p.blend.unwrap_or(true),
        ))
    }

    // -- pivots / palette (engine-ready sprites, cohesive colour) --
    #[tool(
        description = "Set a frame's anchor/pivot point [x,y] in document pixels (feet, weapon mount, …) so engines position the sprite. Omit `pivot` to clear it. Exported (scaled) in sheet/atlas JSON."
    )]
    async fn doc_set_pivot(&self, Parameters(p): Parameters<DocSetPivot>) -> String {
        let pivot = match &p.pivot {
            Some(v) if v.len() >= 2 => Some([v[0], v[1]]),
            Some(_) => return j(json!({"error": "pivot must be [x,y]"})),
            None => None,
        };
        res(self.studio().doc_set_pivot(&p.doc_id, p.frame, pivot))
    }

    #[tool(
        description = "Set a frame's collision boxes — body/hit/hurt rects an engine reads straight off the sheet. Each box is {name, kind:body|hit|hurt, rect:[x,y,w,h]}. Replaces the frame's whole set; pass boxes=[] to clear. Emitted (scaled) in sheet/atlas JSON next to pivot."
    )]
    async fn doc_set_frame_boxes(&self, Parameters(p): Parameters<DocSetFrameBoxes>) -> String {
        let mut boxes = Vec::with_capacity(p.boxes.len());
        for b in &p.boxes {
            if b.rect.len() != 4 {
                return j(json!({"error": format!("box '{}' rect must be [x,y,w,h]", b.name)}));
            }
            boxes.push(crate::document::BoxMeta {
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
    async fn doc_set_palette(&self, Parameters(p): Parameters<DocSetPalette>) -> String {
        let colors: Vec<[u8; 4]> = p.colors.iter().map(|c| rgba(c)).collect();
        res(self.studio().doc_set_palette(&p.doc_id, colors))
    }

    #[tool(
        description = "Recolour a whole document in one call: swap each `from[i]` colour to `to[i]` across every cel (exact match, all channels), updating the stored palette too. `from`/`to` are equal-length lists of [r,g,b]/[r,g,b,a]. Optional `layer`/`frame` restrict scope. The recolour-variant workflow (one sprite, many palettes). Returns {changed}."
    )]
    async fn doc_palette_swap(&self, Parameters(p): Parameters<DocPaletteSwap>) -> String {
        let from: Vec<[u8; 4]> = p.from.iter().map(|c| rgba(c)).collect();
        let to: Vec<[u8; 4]> = p.to.iter().map(|c| rgba(c)).collect();
        res(self
            .studio()
            .doc_palette_swap(&p.doc_id, from, to, p.layer, p.frame))
    }

    #[tool(
        description = "Apply MANY ordered drawing ops to one cel in a single call (fast headless editing). Each op is an object {\"op\":\"rect|line|ellipse|polyline|polygon|bezier|pencil|text|fill|replace_color|flip|shift|outline|fill_cel|clear_cel|gradient|scatter|noise|adjust|blur|quantize|symmetry|drop_shadow|glow|bevel|shade|dither|pixel_perfect\", ...} taking the same fields as the matching tool. Add per-op \"opacity\" (0..255) and/or \"blend_mode\" to composite that op instead of overwriting. Honours an active doc_select."
    )]
    async fn doc_batch(&self, Parameters(p): Parameters<DocBatch>) -> String {
        res(self.studio().doc_batch(&p.doc_id, p.layer, p.frame, p.ops))
    }

    // -- world-class-art tools (see docs/ART-QUALITY-REVIEW.md) --
    #[tool(
        description = "SEE a frame as an INLINE PNG (no separate file read) plus measured stats — the agent's primary eye; use this instead of doc_render when you want to look. mode: render | value/grayscale | bands | sat | hue | notan (3-value squint). grid + coords burn a pixel ruler into the upscale; onion ghosts neighbours; region crops; max_size makes a thumbnail. Stats report value min/max/mean/contrast and shadow/mid/light mass %."
    )]
    async fn doc_look(&self, Parameters(p): Parameters<DocLook>) -> CallToolResult {
        img_result(self.studio().look(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.scale.unwrap_or(6),
            region(&p.region),
            p.mode.as_deref().unwrap_or("render"),
            p.bands.unwrap_or(4),
            p.grid.unwrap_or(false),
            p.coords.unwrap_or(false),
            p.onion.unwrap_or(false),
            p.max_size,
        ))
    }

    #[tool(
        description = "Render the ACTIVE selection as a quick-mask overlay (selected art shown, the rest dimmed + magenta-tinted) so you never paint through an unseen mask. Returns an inline PNG + selected-pixel count and bbox."
    )]
    async fn doc_select_render(&self, Parameters(p): Parameters<DocSelectRender>) -> CallToolResult {
        img_result(self.studio().select_render(&p.doc_id, p.scale.unwrap_or(6)))
    }

    #[tool(
        description = "Document history for an all-destructive editor. action: save (snapshot the doc) | list | restore (roll back) | diff (regression deltas vs a snapshot: pixel/colour/contrast change, added/removed/recoloured) | prune. Snapshot before a risky op (form/quantize/relight/fill) and restore if it gets worse."
    )]
    async fn doc_checkpoint(&self, Parameters(p): Parameters<DocCheckpoint>) -> String {
        res(self.studio().checkpoint(
            &p.doc_id,
            &p.action,
            p.label.as_deref(),
            p.checkpoint_id.as_deref(),
        ))
    }

    #[tool(
        description = "Layer-stack lifecycle in one tool. action: move (needs to_index) | insert (empty layer at index) | delete | rename (needs name) | duplicate | merge_down (bake a layer's opacity+blend onto the one below it). Cels follow the layer. Returns the new layer list."
    )]
    async fn doc_layer_ops(&self, Parameters(p): Parameters<DocLayerOps>) -> String {
        res(self.studio().layer_ops(
            &p.doc_id,
            &p.action,
            p.index.unwrap_or(0),
            p.to_index,
            p.name,
            p.opacity.unwrap_or(255),
            p.blend.unwrap_or_else(|| "normal".into()),
        ))
    }

    #[tool(
        description = "Generate a PERCEPTUALLY-EVEN shading ramp in OKLCh (fixes linear-HSL's crushed midtones): equal perceptual-lightness steps between value_lo..value_hi (OKLab L, 0..1; defaults bracket the base), total hue_shift° across the ramp (light warm / dark cool), sat_curve flat|arc|sat-in-shadow, anchor_midtone forces the centre step to be exactly base. Reports an evenness validation; set_doc stores it as that document's palette."
    )]
    async fn doc_make_perceptual_ramp(
        &self,
        Parameters(p): Parameters<DocPerceptualRamp>,
    ) -> String {
        res(self.studio().make_perceptual_ramp(
            rgba(&p.base),
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
        description = "Snap a cel (or the whole document, if layer/frame omitted) to its locked palette by PERCEPTUALLY nearest colour (OKLab ΔE) — kills the off-palette drift that blends/dithers/glow/gradient leave behind. `palette` overrides the stored one. Returns the pixel count moved."
    )]
    async fn doc_snap_palette(&self, Parameters(p): Parameters<DocSnapPalette>) -> String {
        res(self.studio().snap_palette(
            &p.doc_id,
            p.layer,
            p.frame,
            p.palette.map(|v| palette_list(&v)),
        ))
    }

    #[tool(
        description = "Contiguous magic-wand → the active selection mask. Floods from (x,y) over same-colour pixels (perceptual OKLab tolerance by default; `conn8` for 8-connectivity). `layer` omitted samples the flattened composite. `mode` combines with the current selection: replace|add|subtract|intersect. The precondition for local recolour/re-shade; pair with doc_select_render to SEE the mask."
    )]
    async fn doc_select_wand(&self, Parameters(p): Parameters<DocSelectWand>) -> String {
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
    async fn doc_smooth_edges(&self, Parameters(p): Parameters<DocSmoothEdges>) -> String {
        res(self.studio().smooth_edges(
            &p.doc_id,
            p.layer,
            p.frame,
            p.ramp.map(|v| palette_list(&v)),
            p.max_run.unwrap_or(2),
            p.only_color.as_deref().map(rgba),
            region(&p.region),
        ))
    }

    #[tool(
        description = "Affine-transform a cel (or `region`) IN PLACE about its centre — the #1 missing primitive: rotate degrees, scale_x/scale_y, skew_x/skew_y degrees. method rotsprite (super-sampled, keeps clusters from shattering) | nearest. preserve_volume derives scale_y=1/scale_x for squash-and-stretch. snap_palette re-snaps the transform fringe; clear_source makes it a move. Returns placed bbox + pixel counts."
    )]
    async fn doc_transform_cel(&self, Parameters(p): Parameters<DocTransformCel>) -> String {
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
            p.snap_palette.unwrap_or(false),
            p.clear_source.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Art-director scorecard: the named pixel-art failure modes the agent can't see — orphan specks, un-AA'd jaggies (outer step corners), low contrast, pillow-shading (light pooled at the centre with no direction), value-soup massing, and off-palette drift. Verdicts are conservative (ok|warn|info) with worst-offending cells so you can fix locally. Snapshot with doc_checkpoint first if acting on it."
    )]
    async fn doc_critique(&self, Parameters(p): Parameters<DocCritique>) -> String {
        res(self
            .studio()
            .critique(&p.doc_id, p.frame.unwrap_or(0), p.layer, region(&p.region)))
    }

    #[tool(
        description = "Multi-light form shading — key/fill/rim, the leap from one-direction `form` to PAINTED form. Reads the silhouette as a height field, derives surface normals (bulge = how domed), and lights it: key by azimuth (0=right,90=down,180=left,270=up) + elevation (0=grazing,90=head-on), an auto fill opposite the key, a Fresnel rim, and ambient. Output multiplies the base colour (hue preserved, light colour tints) or snaps to `ramp`. Honours an active selection; pass a region on multi-material sprites."
    )]
    async fn doc_relight(&self, Parameters(p): Parameters<DocRelight>) -> String {
        res(self.studio().relight(
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
            p.ambient_color.as_deref().map(rgb3).unwrap_or([120, 130, 170]),
            p.bulge.unwrap_or(2.0),
            p.ramp.map(|v| palette_list(&v)),
        ))
    }

    #[tool(
        description = "Graduated multi-tone dithering across a whole RAMP along an axis (h|v|radial) — master gradient shading, vs the two-colour `dither`. pattern bayer2/4/8 | checker | ign (blue-noise, no visible matrix grid). only_existing repaints just opaque pixels (shade existing art, keep alpha). Honours an active selection. Snap afterwards with doc_snap_palette if it drifts."
    )]
    async fn doc_dither_ramp(&self, Parameters(p): Parameters<DocDitherRamp>) -> String {
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
        description = "Every frame in ONE labelled inline grid (index + duration) — the animator's flip-test the agent can't otherwise do. cols sets the grid width; scale upscales each frame."
    )]
    async fn doc_contact_sheet(&self, Parameters(p): Parameters<DocContactSheet>) -> CallToolResult {
        img_result(
            self.studio()
                .contact_sheet(&p.doc_id, p.scale.unwrap_or(4), p.cols.unwrap_or(8)),
        )
    }

    #[tool(
        description = "Build a HARMONIOUS multi-ramp palette in OKLCh: one perceptual ramp per hue of a colour scheme (complementary | triadic | analogous | split | tetradic | mono), all sharing lightness poles so the set reads as one cohesive palette. set_doc stores the flattened palette on that document."
    )]
    async fn doc_harmony_palette(&self, Parameters(p): Parameters<DocHarmonyPalette>) -> String {
        res(self.studio().harmony_palette(
            rgba(&p.base),
            p.scheme.as_deref().unwrap_or("complementary"),
            p.per_ramp.unwrap_or(5),
            p.value_lo,
            p.value_hi,
            p.hue_shift.unwrap_or(20.0),
            p.set_doc.as_deref(),
        ))
    }

    #[tool(
        description = "Draw a shaded isometric cuboid (top + two side faces) from one base colour, auto-shaded along a perceptual ramp — the hard-surface form primitive `form` can't make (crates, blocks, buildings, dice). (cx,cy) = centre of the top diamond, s = its half-width, ht = body height; light_right brightens the right face."
    )]
    async fn doc_box(&self, Parameters(p): Parameters<DocBox>) -> String {
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
        description = "Add a faint, non-destructive guide layer to construct against, then delete with doc_layer_ops. kind: thirds (rule-of-thirds) | grid (square, `spacing`) | iso (2:1 lattice) | vp (rays from a vanishing point `vp`=[x,y]). Pure construction scaffolding — perspective, iso, and composition."
    )]
    async fn doc_perspective_guide(
        &self,
        Parameters(p): Parameters<DocPerspectiveGuide>,
    ) -> String {
        let vp = p
            .vp
            .as_ref()
            .filter(|v| v.len() >= 2)
            .map(|v| (v[0], v[1]));
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
    ) -> String {
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
    async fn doc_material(&self, Parameters(p): Parameters<DocMaterial>) -> String {
        res(self.studio().material(
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
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Atelier {
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
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await?;
        if let Some((recorder, tool, args)) = recording {
            if !is_error_result(&result) {
                recorder.record(&tool, args);
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
            "atelier: a headless pixel-art editor (Aseprite-as-API). doc_create a \
             layered/animated document, then paint cels with doc_pencil/line/rect/ \
             ellipse/fill/outline (or doc_batch for many ops at once). Call doc_render \
             to flatten a frame to a PNG you can SEE, inspect it, and iterate. Export \
             with doc_export_sheet / doc_export_gif, or export_all to bundle every \
             document. list_docs to browse the library."
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
load();
setInterval(refreshImages, 2000);
setInterval(load, 10000);
</script>
</body>
</html>"#;

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
        move || {
            let mut atelier = Atelier::with_studio(studio.clone());
            atelier.recorder = recorder.clone();
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
        .route("/gallery/docs", axum::routing::get(gallery_docs))
        .route(
            "/gallery/{id}/render.png",
            axum::routing::get(gallery_render),
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
        let recipe = crate::replay::Recipe::parse(&src).expect("recipe parses");

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
        let recipe = crate::replay::Recipe::parse(&src).expect("recipe parses");
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
        assert_eq!(gallery_scale(None), 4); // absent -> doc_render default
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
