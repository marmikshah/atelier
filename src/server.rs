//! atelier MCP server (rmcp). Exposes the headless document editor as MCP
//! tools over stdio or Streamable HTTP. Tools return JSON strings; studio errors
//! come back as {"error": ...} payloads rather than failing the call.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
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
pub struct DocSetPalette {
    pub doc_id: String,
    /// Palette swatches, each [r,g,b] or [r,g,b,a].
    pub colors: Vec<Vec<i64>>,
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
    /// "linear", "ease-in", "ease-out" or "ease-in-out" (cubic).
    pub easing: Option<String>,
    /// Clear the original rect in each destination frame first (default true).
    pub clear_source: Option<bool>,
}

// --- server ----------------------------------------------------------------

#[derive(Clone)]
pub struct Atelier {
    /// Shared so concurrent HTTP sessions serialise document file writes.
    studio: std::sync::Arc<std::sync::Mutex<Studio>>,
    tool_router: ToolRouter<Self>,
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
        }
    }

    fn studio(&self) -> std::sync::MutexGuard<'_, Studio> {
        self.studio.lock().expect("studio lock poisoned")
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
        description = "Eased multi-frame region motion. Reads the `region` [x0,y0,x1,y1] content from `from_frame` and stamps it (source-over) into every frame in (from_frame, to_frame] at an eased fraction of the total (dx,dy); to_frame gets the full offset. easing: linear/ease-in/ease-out/ease-in-out (cubic). clear_source=true (default) clears the original rect in each destination frame so a moving limb leaves no stale copy. Frames must already exist (else error — doc_add_frame first). Returns frames_touched + per-frame offsets."
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
        description = "Set the document's palette: a list of [r,g,b]/[r,g,b,a] swatches. Stored on the doc and emitted in exports — lock a cohesive N-colour set across sprites."
    )]
    async fn doc_set_palette(&self, Parameters(p): Parameters<DocSetPalette>) -> String {
        let colors: Vec<[u8; 4]> = p.colors.iter().map(|c| rgba(c)).collect();
        res(self.studio().doc_set_palette(&p.doc_id, colors))
    }

    #[tool(
        description = "Apply MANY ordered drawing ops to one cel in a single call (fast headless editing). Each op is an object {\"op\":\"rect|line|ellipse|polyline|polygon|bezier|pencil|fill|replace_color|flip|shift|outline|fill_cel|clear_cel|gradient|scatter|noise|adjust|blur|quantize|symmetry|drop_shadow|glow|bevel|shade|dither|pixel_perfect\", ...} taking the same fields as the matching tool. Add per-op \"opacity\" (0..255) and/or \"blend_mode\" to composite that op instead of overwriting. Honours an active doc_select."
    )]
    async fn doc_batch(&self, Parameters(p): Parameters<DocBatch>) -> String {
        res(self.studio().doc_batch(&p.doc_id, p.layer, p.frame, p.ops))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Atelier {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
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

/// Run over stdio (default transport).
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let service = Atelier::new().serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
