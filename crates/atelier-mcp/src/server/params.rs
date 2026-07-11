//! Tool parameter structs (`Deserialize` + `JsonSchema`) advertised by the
//! `#[tool]` router in `server`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

// --- library params --------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocCreate {
    pub(crate) name: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRef {
    pub(crate) doc_id: String,
}

#[derive(Deserialize, JsonSchema, Default)]
pub(crate) struct ListDocs {
    /// Keep documents whose id starts with this (family selector, e.g. "hero-").
    pub(crate) prefix: Option<String>,
    /// Keep documents whose id contains this substring.
    pub(crate) contains: Option<String>,
}

#[derive(Deserialize, JsonSchema, Default)]
pub(crate) struct DocSetAudit {
    /// Explicit member document ids (combined with `prefix` as a union).
    pub(crate) ids: Option<Vec<String>>,
    /// Select every document whose id starts with this (e.g. "hero-").
    pub(crate) prefix: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSetPaletteSync {
    /// Explicit target document ids (combined with `prefix` as a union).
    pub(crate) ids: Option<Vec<String>>,
    /// Select every document whose id starts with this (e.g. "hero-").
    pub(crate) prefix: Option<String>,
    /// The palette to broadcast, as [[r,g,b(,a)],...]. Or use `from_doc`.
    pub(crate) palette: Option<Vec<Vec<i64>>>,
    /// Copy the locked palette from this document instead of passing colours.
    pub(crate) from_doc: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ExportAll {
    pub(crate) target_dir: String,
    pub(crate) scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ExportAtlas {
    pub(crate) out_path: String,
    pub(crate) scale: Option<u32>,
    /// Max atlas width in pixels before the shelf packer wraps to a new row.
    pub(crate) max_width: Option<u32>,
}

// --- document params -------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocAddTag {
    pub(crate) doc_id: String,
    pub(crate) name: String,
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) direction: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocCel {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocStampImage {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    pub(crate) x: Option<i32>,
    pub(crate) y: Option<i32>,
    pub(crate) png_path: String,
    /// Scale factor (default 1.0). Shrinking uses area-average (features
    /// survive); growing uses nearest (stays crisp).
    pub(crate) scale: Option<f32>,
    /// Scale to this width in pixels instead (wins over `scale`) — e.g. stamp
    /// a 1024px reference at 32px wide without computing the factor.
    pub(crate) target_w: Option<u32>,
    /// Rotation in degrees, clockwise (default 0).
    pub(crate) rotate: Option<f32>,
    /// Opacity 0..255 when compositing (default 255).
    pub(crate) opacity: Option<u8>,
    /// Blend mode when compositing (default "normal").
    pub(crate) blend: Option<String>,
    /// true overwrites the whole cel; false (default) composites OVER it.
    pub(crate) replace: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocWangTiles {
    pub(crate) doc_id: String,
    /// Tile size N in pixels; the source canvas must be at least N×N.
    pub(crate) n: u32,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocNineSlice {
    pub(crate) doc_id: String,
    /// Destination layer / frame.
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Source panel layer / frame (defaults: same layer, frame 0).
    pub(crate) src_layer: Option<usize>,
    pub(crate) src_frame: Option<usize>,
    /// Source panel rect [x, y, w, h] — the panel authored once.
    pub(crate) src: Vec<i64>,
    /// Corner thickness in px (the 3×3 cut; default 3).
    pub(crate) inset: Option<i64>,
    /// Destination rect [x, y, w, h] — any size ≥ the corners.
    pub(crate) dst: Vec<i64>,
    /// How edges/centre fill: "tile" (default) or "stretch".
    pub(crate) mode: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocEmit {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    /// Emitter rect [x, y, w, h] particles spawn inside.
    pub(crate) region: Vec<i64>,
    /// Frame count (default 8, clamped 2..=24).
    pub(crate) frames: Option<usize>,
    /// Particle count (default 24, clamped 1..=512).
    pub(crate) count: Option<usize>,
    /// Emission direction in degrees (0=right, 90=down, 270=up; default 270).
    pub(crate) angle: Option<f32>,
    /// Half-cone spread around `angle` in degrees (default 30).
    pub(crate) spread: Option<f32>,
    /// Initial speed in px/frame (default 1.5).
    pub(crate) speed: Option<f32>,
    /// Downward acceleration in px/frame² (negative = rises; default 0).
    pub(crate) gravity: Option<f32>,
    /// Particle lifetime in loop units — 1.0 = lives one full cycle (default 1).
    pub(crate) life: Option<f32>,
    /// Particle size in px at birth (shrinks as it dies; default 2).
    pub(crate) size: Option<i64>,
    /// Deterministic seed (default 1).
    pub(crate) seed: Option<u64>,
    /// Base colour [r,g,b(,a)] — auto-ramped unless `ramp` given.
    pub(crate) color: Vec<i64>,
    /// Explicit birth→death colour ramp [[r,g,b,a],...].
    pub(crate) ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocColorblindCheck {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    /// Nearest-neighbour upscale of the returned strip (default 2, clamped 1..=8).
    pub(crate) scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocAutotileSet {
    pub(crate) doc_id: String,
    /// Tile size N in pixels; the source canvas must be at least N×N.
    pub(crate) n: u32,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocTilemapAssemble {
    pub(crate) doc_id: String,
    /// Tile size N in pixels; the source canvas must be at least N×N.
    pub(crate) n: u32,
    /// The terrain mask, one string per map row: `#`/`1`/`x` = filled cell,
    /// anything else = empty. Short rows pad with empty.
    pub(crate) rows: Vec<String>,
    /// How off-map cells read: "filled" (terrain continues past the edge,
    /// default) or "empty" (map borders get outlines).
    pub(crate) outside: Option<String>,
}

// --- drawing params --------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocTween {
    pub(crate) doc_id: String,
    pub(crate) from: usize,
    pub(crate) to: usize,
    /// In-between frames to insert (default 1).
    pub(crate) steps: Option<usize>,
    /// Duration of each inserted frame in ms (default 100).
    pub(crate) duration_ms: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocGlow {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Glow tint; omit to bloom the art's own colours.
    pub(crate) color: Option<Vec<i64>>,
    /// Blur radius (default 2).
    pub(crate) radius: Option<i32>,
    /// Glow strength 0..255 (default 180).
    pub(crate) intensity: Option<u8>,
    /// Blend mode for the bloom (default "screen"; "add" for hotter).
    pub(crate) mode: Option<String>,
    /// When a palette is locked, re-snap the bloom back ON-palette afterwards so
    /// it stays crisp pixel art instead of hundreds of soft off-palette tints
    /// (the bloom is binarised at `snap_cutoff`). Default true when a palette is
    /// set; pass false to keep a deliberately soft bloom.
    pub(crate) snap: Option<bool>,
    /// Alpha cutoff for the post-glow snap (0..255, default 64 — keeps the
    /// brighter bloom core, drops the faint halo).
    pub(crate) snap_cutoff: Option<u8>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRimLight {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Rim colour [r,g,b(,a)].
    pub(crate) color: Vec<i64>,
    /// Light azimuth in degrees: 0=right, 90=down, 180=left, 270=up.
    pub(crate) az: f32,
    /// Rim band thickness in pixels (default 1).
    pub(crate) width: Option<i32>,
    /// Facing falloff exponent — higher = tighter rim (default 1.5).
    pub(crate) falloff: Option<f32>,
    /// Light the AWAY-facing edge instead (core/contact shadow). Default false.
    pub(crate) dark: Option<bool>,
    /// Snap on-palette afterwards when a palette is locked (default true).
    pub(crate) snap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocCastShadow {
    pub(crate) doc_id: String,
    /// The caster layer (its silhouette throws the shadow).
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Shadow colour [r,g,b(,a)].
    pub(crate) color: Vec<i64>,
    /// Light azimuth in degrees: 0=right, 90=down, 180=left, 270=up (pairs with
    /// the vector doc_form_audit infers). The shadow falls opposite. Default 135.
    pub(crate) az: Option<f32>,
    /// Ground stretch — how far the shadow projects (default 1.0).
    pub(crate) length: Option<f32>,
    /// How much height survives, 0..1 (0 = flat on the ground; default 0.2).
    pub(crate) squash: Option<f32>,
    /// Shadow strength 0..255 (default 140).
    pub(crate) opacity: Option<u8>,
    /// Paint the shadow onto this layer and clip it to that layer's opaque
    /// pixels (the ground surface). Omit = draw behind the caster on its own cel.
    pub(crate) receiver_layer: Option<usize>,
    /// Snap on-palette afterwards when a palette is locked (default true).
    pub(crate) snap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSelect {
    pub(crate) doc_id: String,
    /// "rect" | "ellipse" | "color" | "all" | "none". Default "rect".
    pub(crate) shape: Option<String>,
    /// Combine with the current selection: "replace" (default) | "add" |
    /// "subtract" | "intersect".
    pub(crate) mode: Option<String>,
    /// rect shape:
    pub(crate) x0: Option<i32>,
    pub(crate) y0: Option<i32>,
    pub(crate) x1: Option<i32>,
    pub(crate) y1: Option<i32>,
    /// ellipse shape:
    pub(crate) cx: Option<i32>,
    pub(crate) cy: Option<i32>,
    pub(crate) rx: Option<i32>,
    pub(crate) ry: Option<i32>,
    /// color shape — which cel to test, and either an explicit `color` or a
    /// sample point (`x`,`y`) plus `tolerance` (max channel distance).
    pub(crate) layer: Option<usize>,
    pub(crate) frame: Option<usize>,
    pub(crate) color: Option<Vec<i64>>,
    pub(crate) x: Option<i32>,
    pub(crate) y: Option<i32>,
    pub(crate) tolerance: Option<i32>,
}

/// A frame's anchor point (pivot) — set or clear it.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSetPivot {
    pub(crate) doc_id: String,
    pub(crate) frame: usize,
    /// Anchor point [x,y] in document pixels; omit to clear the pivot.
    pub(crate) pivot: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct BoxInput {
    /// Label for this box (e.g. "torso", "sword", "feet").
    pub(crate) name: String,
    /// `body` (collision), `hit` (deals damage) or `hurt` (takes damage).
    pub(crate) kind: String,
    /// `[x, y, w, h]` in document pixels.
    pub(crate) rect: Vec<i32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSetFrameBoxes {
    pub(crate) doc_id: String,
    pub(crate) frame: usize,
    /// The frame's full box list; pass `[]` to clear. Replaces any existing set.
    pub(crate) boxes: Vec<BoxInput>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSetPalette {
    pub(crate) doc_id: String,
    /// Palette swatches, each [r,g,b] or [r,g,b,a].
    pub(crate) colors: Vec<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocPaletteSwap {
    pub(crate) doc_id: String,
    /// Source colours to recolour, each [r,g,b]/[r,g,b,a]. Matched exactly
    /// (all channels). Same length as `to` — `from[i]` becomes `to[i]`.
    pub(crate) from: Vec<Vec<i64>>,
    /// Replacement colours, each [r,g,b]/[r,g,b,a]. Same length as `from`.
    pub(crate) to: Vec<Vec<i64>>,
    /// Restrict to one layer's cels; omit for every layer.
    pub(crate) layer: Option<usize>,
    /// Restrict to one frame's cels; omit for every frame.
    pub(crate) frame: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocBatch {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Ordered ops, each like {"op":"rect","x0":1,"y0":1,"x1":8,"y1":8,"color":[r,g,b],"fill":true}.
    /// Ops: pencil/line/rect/ellipse/polyline/polygon/fill/replace_color/
    /// flip/shift/outline/fill_cel/clear_cel/gradient/scatter/noise/adjust/blur/
    /// quantize/symmetry/drop_shadow/glow/bevel/shade/dither/pixel_perfect.
    pub(crate) ops: Vec<serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocDraw {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// One draw op: pencil | line | rect | ellipse | polyline | polygon | stroke
    /// | fill | gradient | scatter | noise | text | fill_cel.
    pub(crate) op: String,
    /// The op's own params, flattened alongside (e.g. for "rect": x0, y0, x1, y1,
    /// color, fill). Every op also accepts `opacity` and `blend_mode`.
    #[serde(flatten)]
    pub(crate) params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocFx {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// One transform/effect op: blur | outline | drop_shadow | bevel | shade |
    /// form | dither | pixel_perfect | flip | shift | symmetry | quantize |
    /// replace_color | adjust.
    pub(crate) op: String,
    /// The op's own params, flattened alongside. Every op also accepts `opacity`
    /// and `blend_mode`.
    #[serde(flatten)]
    pub(crate) params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocExport {
    pub(crate) doc_id: String,
    /// What to export: sheet | anim | tileset.
    pub(crate) op: String,
    /// Output file path.
    pub(crate) out_path: String,
    /// Nearest-neighbour upscale (sheet/anim default 4, tileset default 1).
    pub(crate) scale: Option<u32>,
    /// Op-specific params, flattened: anim → format ("gif"|"apng"), tag;
    /// tileset → tile_w, tile_h.
    #[serde(flatten)]
    pub(crate) params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocLayer {
    pub(crate) doc_id: String,
    /// add (new layer on top) | set (visibility/opacity/blend of layer `index`) |
    /// move | insert | delete | rename | duplicate | merge_down.
    pub(crate) op: String,
    /// Target layer index (the layer for `set`/`move`/`delete`/…).
    pub(crate) index: Option<usize>,
    /// Destination index for `move`.
    pub(crate) to_index: Option<usize>,
    /// Layer name for `add`/`insert`/`rename`.
    pub(crate) name: Option<String>,
    /// Visibility for `set`.
    pub(crate) visible: Option<bool>,
    pub(crate) opacity: Option<u8>,
    pub(crate) blend: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocFrame {
    pub(crate) doc_id: String,
    /// add (append) | duration (set frame timing) | delete | insert | duplicate |
    /// move.
    pub(crate) op: String,
    /// Target frame index (for duration/delete/insert/duplicate/move).
    pub(crate) frame: Option<usize>,
    /// For `add`: duplicate this frame's cels into the new frame.
    pub(crate) copy_from: Option<usize>,
    /// Destination index for `move`.
    pub(crate) to_index: Option<usize>,
    /// Frame duration in ms (for add/insert/duration; default 100).
    pub(crate) duration_ms: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRegion {
    pub(crate) doc_id: String,
    /// copy | cut | clear | move | paste.
    pub(crate) op: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Source rect for copy/cut/clear/move.
    pub(crate) x0: Option<i32>,
    pub(crate) y0: Option<i32>,
    pub(crate) x1: Option<i32>,
    pub(crate) y1: Option<i32>,
    /// Offset for `move`.
    pub(crate) dx: Option<i32>,
    pub(crate) dy: Option<i32>,
    /// Destination top-left for `paste`.
    pub(crate) x: Option<i32>,
    pub(crate) y: Option<i32>,
    /// `paste`: true = source-over (default), false = overwrite.
    pub(crate) blend: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRefOp {
    pub(crate) doc_id: String,
    /// set (attach/clear the comparison reference) | import (trace an external
    /// image cleaned onto a guide layer).
    pub(crate) op: String,
    /// `set`: reference image path (omit to clear). `import`: source image path.
    pub(crate) path: Option<String>,
    // -- import params --
    pub(crate) layer: Option<usize>,
    pub(crate) frame: Option<usize>,
    /// `import`: target width in px (required for import).
    pub(crate) target_w: Option<u32>,
    /// `import`: omit to derive an aspect-true height.
    pub(crate) target_h: Option<u32>,
    pub(crate) colors: Option<usize>,
    pub(crate) dither: Option<bool>,
    pub(crate) defringe: Option<bool>,
    pub(crate) to_doc_palette: Option<bool>,
    /// `import`: corner-seeded background removal before palette extraction.
    pub(crate) remove_bg: Option<bool>,
    /// `import`: colours the derived palette must keep (e.g. a black outline).
    pub(crate) pin: Option<Vec<Vec<i64>>>,
}

// --- canvas reader params --------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocDumpRegion {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    /// Dump this layer's cel; omit to dump the flattened composite.
    pub(crate) layer: Option<usize>,
    /// [x0,y0,x1,y1] document pixels (inclusive). Omit = whole canvas. Area capped at 4096 px.
    pub(crate) region: Option<Vec<i32>>,
    /// "symbol" (A..Z a..z 0..9 per colour, `.`=transparent) or "hex".
    pub(crate) mode: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSilhouette {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    /// Minimum alpha counted as opaque (default 1).
    pub(crate) alpha_threshold: Option<u8>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocComponents {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    /// Pixel adjacency: 4 or 8 (default 8).
    pub(crate) connectivity: Option<u8>,
    /// Only components of this exact [r,g,b]/[r,g,b,a]; omit = any opaque pixel.
    pub(crate) color: Option<Vec<i64>>,
    /// Components smaller than this are dropped from the list (default 1).
    pub(crate) min_area: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocFormAudit {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    /// Forms smaller than this many pixels are skipped (default 12).
    pub(crate) min_area: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocCoverageMap {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    /// Grid columns (default 8).
    pub(crate) cols: Option<u32>,
    /// Grid rows (default 8).
    pub(crate) rows: Option<u32>,
}

// --- value & colour feedback params ----------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocContrastCheck {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    /// "region" (inside vs 4px surround), "palette" (all colour pairs) or "one-bit".
    pub(crate) mode: String,
    /// [x0,y0,x1,y1] document pixels — required for mode="region".
    pub(crate) region: Option<Vec<i32>>,
    /// WCAG ratio at/above which a pair passes (default 1.5).
    pub(crate) min_ratio: Option<f32>,
    /// Luma cutoff for mode="one-bit" (default 128).
    pub(crate) threshold: Option<u8>,
    /// Where to write the B/W PNG for mode="one-bit".
    pub(crate) out_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocPaletteReport {
    pub(crate) doc_id: String,
    /// One frame; omit to tally every frame.
    pub(crate) frame: Option<usize>,
    /// One layer's cel; omit for the flattened composite per frame.
    pub(crate) layer: Option<usize>,
    /// [x0,y0,x1,y1] document pixels to restrict the tally; omit = whole canvas.
    pub(crate) region: Option<Vec<i32>>,
    /// Max channel distance counting two colours as near-duplicates (default 8).
    pub(crate) dupe_threshold: Option<i32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRampValidate {
    /// Explicit ramp [[r,g,b],...] (≥2). Provide this OR `doc_id`.
    pub(crate) colors: Option<Vec<Vec<i64>>>,
    /// Validate this document's locked palette instead of explicit colours.
    pub(crate) doc_id: Option<String>,
    /// [start,end) slice of the palette to validate (with `doc_id`).
    pub(crate) slice: Option<Vec<usize>>,
}

// --- animation & tiling feedback params ------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocFrameDiff {
    pub(crate) doc_id: String,
    pub(crate) frame_a: usize,
    pub(crate) frame_b: usize,
    /// Diff this layer's cel; omit for the flattened composite.
    pub(crate) layer: Option<usize>,
    /// [x0,y0,x1,y1] document pixels; omit = whole canvas.
    pub(crate) region: Option<Vec<i32>>,
    /// Add a text grid (`.`unchanged `+`added `-`removed `~`recolored); area capped 4096 px.
    pub(crate) grid: Option<bool>,
    /// "none" or "overlay" (frame_b dimmed with changed pixels flagged).
    pub(crate) render: Option<String>,
    pub(crate) out_path: Option<String>,
    pub(crate) scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSeamReport {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    /// Test this layer's cel; omit for the flattened composite.
    pub(crate) layer: Option<usize>,
    /// "both", "horizontal" (left↔right) or "vertical" (top↔bottom).
    pub(crate) axis: Option<String>,
    /// Max per-channel delta still counted as a matching edge (default 0).
    pub(crate) threshold: Option<i32>,
    /// Render a PNG with mismatched edge pixels highlighted red; returns its path.
    pub(crate) out_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocAnimAudit {
    pub(crate) doc_id: String,
    /// Audit this tag's loop; omit to audit the whole timeline.
    pub(crate) tag: Option<String>,
    /// Use this layer's cel; omit for the flattened composite.
    pub(crate) layer: Option<usize>,
    /// "seam" (loop wrap diff), "spacing" (per-frame motion evenness),
    /// "arc" (trajectory shape) or "timing" (per-frame durations).
    pub(crate) mode: String,
    /// [x0,y0,x1,y1] clips spacing/arc to one part (e.g. a swinging arm) so
    /// its motion is measurable over a static body.
    pub(crate) region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocKeyframeMove {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    /// Source rect [x0,y0,x1,y1] (document pixels) read from `from_frame`.
    pub(crate) region: Vec<i32>,
    pub(crate) from_frame: usize,
    /// Destination keyframe (> from_frame); all frames between move too.
    pub(crate) to_frame: usize,
    /// Total displacement applied at to_frame; intermediates are eased fractions.
    pub(crate) dx: i32,
    pub(crate) dy: i32,
    /// "linear", "ease-in", "ease-out", "ease-in-out" (cubic), "bounce"
    /// (ease-out bounce), "overshoot" (back ease-out — shoots past the offset
    /// then settles) or "elastic" (decaying oscillation). Default "linear".
    pub(crate) easing: Option<String>,
    /// Clear the original rect in each destination frame first (default true).
    pub(crate) clear_source: Option<bool>,
}

// --- world-class-art tool params (the art-quality pass) --------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocLook {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    pub(crate) scale: Option<u32>,
    /// Inclusive crop corners [x0,y0,x1,y1].
    pub(crate) region: Option<Vec<i32>>,
    /// render | value | bands | sat | hue | notan.
    pub(crate) mode: Option<String>,
    /// Band count for mode=bands.
    pub(crate) bands: Option<u32>,
    pub(crate) grid: Option<bool>,
    pub(crate) coords: Option<bool>,
    pub(crate) onion: Option<bool>,
    pub(crate) max_size: Option<u32>,
    /// Repeat the result N×N to eyeball seamlessness (tileability check).
    pub(crate) tile: Option<u32>,
    /// Also write the PNG to this path, for file/export workflows.
    pub(crate) out_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSelectRender {
    pub(crate) doc_id: String,
    /// Frame whose art backs the mask preview (default 0).
    pub(crate) frame: Option<usize>,
    pub(crate) scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocCheckpoint {
    pub(crate) doc_id: String,
    /// save | list | restore | diff | prune.
    pub(crate) action: String,
    pub(crate) label: Option<String>,
    pub(crate) checkpoint_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSnapPalette {
    pub(crate) doc_id: String,
    pub(crate) layer: Option<usize>,
    pub(crate) frame: Option<usize>,
    /// Override palette as a list of [r,g,b(,a)]; defaults to the doc's palette.
    pub(crate) palette: Option<Vec<Vec<i64>>>,
    /// Partial-alpha policy for FX bloom / AA fringe: `preserve` (default — keep
    /// soft alpha, snap RGB only) | `opaque` (binarise alpha at `cutoff`,
    /// default 128 — collapse a bloom into a crisp on-palette silhouette) |
    /// `flatten` (composite over `bg` then snap fully opaque).
    pub(crate) alpha: Option<String>,
    /// Alpha cutoff for `alpha="opaque"` (0..255, default 128).
    pub(crate) cutoff: Option<u8>,
    /// Backdrop `[r,g,b]` for `alpha="flatten"` (default opaque black).
    pub(crate) bg: Option<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSelectWand {
    pub(crate) doc_id: String,
    /// Layer to sample; omit to sample the flattened composite.
    pub(crate) layer: Option<usize>,
    pub(crate) frame: Option<usize>,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) tol: Option<i32>,
    pub(crate) conn8: Option<bool>,
    pub(crate) perceptual: Option<bool>,
    /// replace | add | subtract | intersect.
    pub(crate) mode: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSmoothEdges {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Optional ramp [[r,g,b],...] to keep AA pixels on-palette.
    pub(crate) ramp: Option<Vec<Vec<i64>>>,
    /// With keep_square, edges flat-straight for longer than this stay crisp.
    pub(crate) max_run: Option<i32>,
    /// Preserve deliberate right-angle corners (default true).
    pub(crate) keep_square: Option<bool>,
    pub(crate) only_color: Option<Vec<i64>>,
    pub(crate) region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocTransformCel {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    pub(crate) region: Option<Vec<i32>>,
    pub(crate) rotate: Option<f32>,
    pub(crate) scale_x: Option<f32>,
    pub(crate) scale_y: Option<f32>,
    pub(crate) skew_x: Option<f32>,
    pub(crate) skew_y: Option<f32>,
    /// rotsprite (cluster-preserving) | nearest.
    pub(crate) method: Option<String>,
    /// With scale_x set and scale_y omitted, derive scale_y = 1/scale_x.
    pub(crate) preserve_volume: Option<bool>,
    /// Snap the resample fringe back to the locked palette (default true; only
    /// acts when a palette is set).
    pub(crate) snap_palette: Option<bool>,
    pub(crate) clear_source: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocCritique {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    pub(crate) region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRelight {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    pub(crate) region: Option<Vec<i32>>,
    /// Key light: azimuth (0=right,90=down,180=left,270=up) + elevation (0..90).
    pub(crate) key_azimuth: Option<f32>,
    pub(crate) key_elevation: Option<f32>,
    pub(crate) key_intensity: Option<f32>,
    pub(crate) key_color: Option<Vec<i64>>,
    pub(crate) fill_intensity: Option<f32>,
    pub(crate) fill_color: Option<Vec<i64>>,
    pub(crate) rim_intensity: Option<f32>,
    pub(crate) rim_color: Option<Vec<i64>>,
    pub(crate) ambient: Option<f32>,
    pub(crate) ambient_color: Option<Vec<i64>>,
    /// How domed the silhouette reads as a form (higher = flatter).
    pub(crate) bulge: Option<f32>,
    pub(crate) ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocDitherRamp {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// The ramp to dither across (>= 2 colours), as [[r,g,b],...].
    pub(crate) ramp: Vec<Vec<i64>>,
    pub(crate) region: Option<Vec<i32>>,
    /// h | v | radial.
    pub(crate) axis: Option<String>,
    /// bayer2 | bayer4 | bayer8 | checker | ign.
    pub(crate) pattern: Option<String>,
    pub(crate) only_existing: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocContactSheet {
    pub(crate) doc_id: String,
    pub(crate) scale: Option<u32>,
    pub(crate) cols: Option<usize>,
    /// Ghost each cell's PREVIOUS frame under it at 35% alpha — per-pair
    /// onion skinning, the closest a still image gets to showing motion.
    pub(crate) onion: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocPalette {
    pub(crate) base: Vec<i64>,
    /// mono | complementary | triadic | analogous | split | tetradic. Default mono.
    pub(crate) scheme: Option<String>,
    /// Colours per ramp (default 5).
    pub(crate) count: Option<usize>,
    pub(crate) value_lo: Option<f32>,
    pub(crate) value_hi: Option<f32>,
    pub(crate) hue_shift: Option<f32>,
    /// flat | arc | sat-in-shadow (default arc).
    pub(crate) sat_curve: Option<String>,
    pub(crate) anchor_midtone: Option<bool>,
    /// Store the flattened palette on this document id.
    pub(crate) set_doc: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocBox {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Centre of the top diamond.
    pub(crate) cx: i32,
    pub(crate) cy: i32,
    /// Half-width of the top diamond.
    pub(crate) s: i32,
    /// Body height.
    pub(crate) ht: i32,
    pub(crate) color: Vec<i64>,
    pub(crate) light_right: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocPerspectiveGuide {
    pub(crate) doc_id: String,
    /// thirds | grid | iso | vp.
    pub(crate) kind: Option<String>,
    pub(crate) color: Option<Vec<i64>>,
    pub(crate) spacing: Option<i32>,
    /// Vanishing point [x,y] for kind=vp.
    pub(crate) vp: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocOutlineSelective {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// from_fill | light | dark.
    pub(crate) mode: Option<String>,
    pub(crate) ramp: Option<Vec<Vec<i64>>>,
    pub(crate) steps: Option<i32>,
    pub(crate) region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocMaterial {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    pub(crate) region: Option<Vec<i32>>,
    /// metal | wood | stone | water | cloth | skin | glass.
    pub(crate) material: String,
    pub(crate) color: Vec<i64>,
    pub(crate) seed: Option<u64>,
    pub(crate) ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocTranslucency {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    pub(crate) region: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocPanel {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
    pub(crate) fill: Vec<i64>,
    pub(crate) border: Option<Vec<i64>>,
    pub(crate) bevel: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocPaintGrid {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Top-left of the grid in document pixels (default 0,0).
    pub(crate) x: Option<i32>,
    pub(crate) y: Option<i32>,
    /// Single-character keys -> [r,g,b(,a)] colour OR an integer palette index
    /// (palette-true by construction). '.' and ' ' are reserved: untouched.
    pub(crate) legend: serde_json::Map<String, Value>,
    /// One string per pixel row, e.g. ["..kk..", ".koook."].
    pub(crate) rows: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocExtractToLayer {
    pub(crate) doc_id: String,
    /// Source layer holding the flat sprite.
    pub(crate) layer: usize,
    /// Frame to read when frames="one" (default 0).
    pub(crate) frame: Option<usize>,
    /// Rect [x0,y0,x1,y1] of the part to cut. Omit to use the active selection.
    pub(crate) region: Option<Vec<i32>>,
    /// Use the active selection mask (set with doc_select / doc_select_wand).
    pub(crate) use_selection: Option<bool>,
    /// Name for the new part layer, e.g. "arm-front".
    pub(crate) name: Option<String>,
    /// "one" (default) cuts just `frame`; "all" cuts every frame's cel.
    pub(crate) frames: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocKeyframeTransform {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    /// Rect [x0,y0,x1,y1] of the part to rotate (document pixels), read from from_frame.
    pub(crate) region: Vec<i32>,
    /// The JOINT the part rotates about, [x,y] in document pixels (e.g. the shoulder).
    pub(crate) pivot: Vec<f32>,
    pub(crate) from_frame: usize,
    /// Last frame of the motion (> from_frame); frames must already exist.
    pub(crate) to_frame: usize,
    /// Total rotation in degrees at to_frame (clockwise positive); intermediates eased.
    pub(crate) rot_deg: Option<f32>,
    /// Total translation applied at to_frame; intermediates eased.
    pub(crate) dx: Option<i32>,
    pub(crate) dy: Option<i32>,
    /// linear | ease-in | ease-out | ease-in-out | bounce | overshoot | elastic.
    pub(crate) easing: Option<String>,
    /// Snap resampled pixels back to the locked palette (default true).
    pub(crate) snap_palette: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRefAnalyze {
    pub(crate) doc_id: String,
    /// Analyze this file instead of the doc's stored reference.
    pub(crate) path: Option<String>,
    /// Width to plan at (default: the canvas width); height derives aspect-true.
    pub(crate) target_w: Option<u32>,
    /// Subject palette size to extract (default 8).
    pub(crate) colors: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRefCompare {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    /// "side_by_side" (default) or "overlay" (reference ghosted under the art).
    pub(crate) mode: Option<String>,
    /// Grid divisions per axis for the ΔE cells (default 8, clamped 2..=16).
    pub(crate) cells: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocDiffMap {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    /// How many worst individual pixels to list (default 20, clamped 1..=64).
    pub(crate) top: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocCritiqueVision {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    /// What to weight the critique toward — e.g. "anatomy", "readability",
    /// "colour", "appeal". Omit for a balanced pass.
    pub(crate) focus: Option<String>,
    /// Render scale handed to the host's vision model (default 8, clamped
    /// 1..=16). Bigger = more pixel detail for the model to read.
    pub(crate) scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocBurst {
    pub(crate) doc_id: String,
    pub(crate) layer: Option<usize>,
    pub(crate) cx: i32,
    pub(crate) cy: i32,
    pub(crate) frames: Option<usize>,
    pub(crate) max_radius: i32,
    /// ring | disc | rays.
    pub(crate) kind: Option<String>,
    pub(crate) color: Vec<i64>,
    pub(crate) ramp: Option<Vec<Vec<i64>>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocFigure {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Named joint coordinates as `{"head":[x,y], "shoulder_l":[x,y], ...}`.
    /// Required: head, shoulder_l/r, elbow_l/r, hand_l/r, hip_l/r, knee_l/r,
    /// foot_l/r. chest and pelvis are derived from the shoulder/hip midpoints.
    pub(crate) joints: std::collections::HashMap<String, Vec<i64>>,
    pub(crate) color: Vec<i64>,
    /// Arm/leg capsule width (default 3).
    pub(crate) limb_w: Option<i64>,
    /// Torso capsule width (default 6).
    pub(crate) torso_w: Option<i64>,
    /// Head radius (default 4).
    pub(crate) head_r: Option<i64>,
    /// Anti-alias the capsule edges (default true).
    pub(crate) aa: Option<bool>,
    /// Snap on-palette afterwards when a palette is locked (default true).
    pub(crate) snap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocPoseCycle {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    /// The base STANDING pose: the same 13 named joints as doc_figure.
    pub(crate) joints: std::collections::HashMap<String, Vec<i64>>,
    /// Which cycle to generate: idle | run | jump | attack | hurt.
    pub(crate) gait: String,
    /// Frame count (omit or 0 = the gait's natural count; clamped 2..=24).
    pub(crate) frames: Option<usize>,
    /// Amplitude multiplier on the gait's motion (default 1.0, clamped 0.1..=3).
    pub(crate) intensity: Option<f32>,
    pub(crate) color: Vec<i64>,
    pub(crate) limb_w: Option<i64>,
    pub(crate) torso_w: Option<i64>,
    pub(crate) head_r: Option<i64>,
    pub(crate) aa: Option<bool>,
    pub(crate) snap: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocWalk {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    /// The base STANDING pose: the same 13 named joints as doc_figure.
    pub(crate) joints: std::collections::HashMap<String, Vec<i64>>,
    pub(crate) color: Vec<i64>,
    /// Number of frames in the cycle (default 8, clamped 2..=24).
    pub(crate) frames: Option<usize>,
    /// Foot front-to-back travel in px (default 10).
    pub(crate) stride: Option<i64>,
    /// Foot lift height on the swing in px (default 4).
    pub(crate) lift: Option<i64>,
    /// Body bob amplitude in px (default 1).
    pub(crate) bob: Option<i64>,
    /// Hand front-to-back swing in px (default 6).
    pub(crate) arm_swing: Option<i64>,
    pub(crate) limb_w: Option<i64>,
    pub(crate) torso_w: Option<i64>,
    pub(crate) head_r: Option<i64>,
    pub(crate) aa: Option<bool>,
    pub(crate) snap: Option<bool>,
}
