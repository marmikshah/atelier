//! Tool parameter structs (`Deserialize` + `JsonSchema`) advertised by the
//! `#[tool]` router in `server`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// Re-type flattened op params a strict tool-call client stringified.
///
/// The single-op tools carry their op's params in a `#[serde(flatten)]` open
/// map, so the advertised schema cannot type them per key — and some clients
/// serialize anything the schema leaves open as a STRING: `color: [255,0,0]`
/// arrives as `"[255, 0, 0]"` and `dx: 2` as `"2"`. The op layer then rejects
/// the colour ("got \"[255, 0, 0]\"") or silently defaults the number — every
/// model in the showcase benchmark hit one of the two. Anything that parses as
/// JSON (array/object/number/bool) is parsed back; `text` is exempt because
/// its value is legitimately prose (drawing the text "42" must stay text).
pub(crate) fn revive_params(params: &mut serde_json::Map<String, Value>) {
    for (key, v) in params.iter_mut() {
        if key == "text" {
            continue;
        }
        if let Value::String(s) = v {
            let t = s.trim();
            let looks_typed = t.starts_with('[')
                || t.starts_with('{')
                || t == "true"
                || t == "false"
                || t.parse::<f64>().is_ok();
            if looks_typed {
                if let Ok(parsed) = serde_json::from_str::<Value>(t) {
                    *v = parsed;
                }
            }
        }
    }
}

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

// --- drawing params --------------------------------------------------------

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
    /// polygon/lasso shape — the traced vertices `[[x,y], ...]` (needs ≥3),
    /// automatically closed (last point joins the first).
    pub(crate) points: Option<Vec<[i32; 2]>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocBatch {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Apply the SAME op list to each of these frames as well (a repeated fix
    /// on a static layer is one call, not one per frame). Runs on the union of
    /// `frame` and this list, each frame in full op order.
    pub(crate) frames: Option<Vec<usize>>,
    /// Ordered ops, each an object; the derived item schema would be `true`
    /// (any), which strict tool-call parsers (e.g. Gemini) reject — so pin it to
    /// `{"type":"object"}` while keeping the free-form op payload.
    #[schemars(schema_with = "op_list_schema")]
    pub(crate) ops: Vec<serde_json::Value>,
}

/// Array-of-object schema for a free-form op list. Emitting a concrete `items`
/// object keeps `doc_batch` callable by tool parsers that refuse a boolean
/// (`items: true`) array schema.
fn op_list_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "items": { "type": "object" },
        "description": "Ordered ops, each like {\"op\":\"rect\",\"x0\":1,\"y0\":1,\"x1\":8,\"y1\":8,\"color\":[r,g,b],\"fill\":true}. Draw ops: pencil/line/rect/ellipse/polyline/polygon/stroke/fill/bucket/gradient/scatter/noise/text/fill_cel/clear_cel. FX ops: blur/outline/drop_shadow/bevel/shade/form/dither/pixel_perfect/flip/shift/symmetry/quantize/replace_color/adjust/gradient_map. Plus glow (batch only). Each takes the same fields as the matching doc_draw/doc_fx op (glow is batch-only)."
    })
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocDraw {
    pub(crate) doc_id: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// One draw op: pencil | line | rect | ellipse | polyline | polygon | stroke
    /// | fill | gradient | scatter | noise | text | fill_cel | clear_cel |
    /// box_iso | panel.
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
    /// replace_color | adjust | gradient_map.
    pub(crate) op: String,
    /// The op's own params, flattened alongside. Every op also accepts `opacity`
    /// and `blend_mode`.
    #[serde(flatten)]
    pub(crate) params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocExport {
    /// Required for per-document ops (sheet | anim | tileset); omit for the
    /// library-wide ops (all | atlas), which span every document.
    pub(crate) doc_id: Option<String>,
    /// Per-document: sheet | anim | tileset. Library-wide: all (one spritesheet
    /// per document into a dir) | atlas (every frame of every document packed
    /// into one atlas PNG + master JSON map).
    pub(crate) op: String,
    /// Output path: a file for sheet/anim/tileset/atlas, a target DIRECTORY for
    /// op=all.
    pub(crate) out_path: String,
    /// Nearest-neighbour upscale (sheet/anim/all/atlas default 4, tileset 1).
    pub(crate) scale: Option<u32>,
    /// Op-specific params, flattened: anim → format ("gif"|"apng"), tag;
    /// tileset → tile_w, tile_h; atlas → max_width (shelf-packer wrap, default 512).
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
    /// For `add`: append this many frames in one call (default 1, max 256).
    pub(crate) count: Option<usize>,
    /// Destination index for `move`.
    pub(crate) to_index: Option<usize>,
    /// Frame duration in ms (for add/insert/duration; default 100).
    pub(crate) duration_ms: Option<u32>,
    /// For `duplicate`: link the new frame's cels to the source's (shared
    /// pixels, copy-on-write on any later edit) instead of copying them.
    pub(crate) link: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocSlice {
    pub(crate) doc_id: String,
    /// add | delete | list.
    pub(crate) op: String,
    /// Slice name (required for add/delete).
    pub(crate) name: Option<String>,
    /// For add: the slice bounds, inclusive corners [x0,y0,x1,y1].
    pub(crate) rect: Option<Vec<i32>>,
    /// 9-slice centre rect [x0,y0,x1,y1] (must sit inside rect).
    pub(crate) center: Option<Vec<i32>>,
    /// Pivot point [x,y] (unconstrained — a rotation origin may sit off-canvas).
    pub(crate) pivot: Option<Vec<i32>>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocTile {
    pub(crate) doc_id: String,
    /// place (stamp a tilemap from a tileset document).
    pub(crate) op: String,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// The tileset document: its flattened frame 0 is sliced row-major into
    /// tile_w×tile_h tiles (index 0 = top-left). Read-only — never modified.
    pub(crate) tiles_doc: String,
    pub(crate) tile_w: u32,
    pub(crate) tile_h: u32,
    /// Cells to stamp: [[cell_x, cell_y, tile_index], ...]; each lands at
    /// pixel (cell_x*tile_w, cell_y*tile_h), source-over, canvas-clipped.
    pub(crate) cells: Vec<Vec<i32>>,
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
    /// Replay-only, hidden from the advertised schema: the pixels a journaled
    /// paste embedded, so a rebuild does not depend on the live clipboard.
    #[schemars(skip)]
    pub(crate) clipboard: Option<ClipboardPixels>,
}

/// The clipboard content a journaled `doc_region op=paste` step carries:
/// base64 RGBA, row-major, `w`×`h`.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct ClipboardPixels {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) data: String,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DocRefOp {
    pub(crate) doc_id: String,
    /// set (attach/clear the comparison reference) | import (trace an external
    /// image cleaned onto a guide layer) | analyze (decompose the reference:
    /// background coverage, subject palette, silhouette grid) | compare (score a
    /// frame against the reference: silhouette IoU + per-cell ΔE) | diff
    /// (per-pixel signed error map vs the reference + worst pixels).
    pub(crate) op: String,
    /// `set`/`analyze`: reference image path (set: omit to clear; analyze: an
    /// external file instead of the stored reference). `import`: source path.
    pub(crate) path: Option<String>,
    // -- import params --
    pub(crate) layer: Option<usize>,
    pub(crate) frame: Option<usize>,
    /// `import`: target width in px (required for import). `analyze`: plan width.
    pub(crate) target_w: Option<u32>,
    /// `import`: omit to derive an aspect-true height.
    pub(crate) target_h: Option<u32>,
    /// `import`/`analyze`: palette size (import default 16, analyze default 8).
    pub(crate) colors: Option<usize>,
    pub(crate) dither: Option<bool>,
    pub(crate) defringe: Option<bool>,
    pub(crate) to_doc_palette: Option<bool>,
    /// `import`: corner-seeded background removal before palette extraction.
    pub(crate) remove_bg: Option<bool>,
    /// `import`: colours the derived palette must keep (e.g. a black outline).
    pub(crate) pin: Option<Vec<Vec<i64>>>,
    // -- compare params --
    /// `compare`: "side_by_side" (default) or "overlay" (reference ghosted under).
    pub(crate) mode: Option<String>,
    /// `compare`: ΔE grid divisions per axis (default 8, clamped 2..=16).
    pub(crate) cells: Option<u32>,
    // -- diff params --
    /// `diff`: worst individual pixels to list (default 20, clamped 1..=64).
    pub(crate) top: Option<usize>,
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

// --- value & colour feedback params ----------------------------------------

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
    /// Matte transparency for viewing: checker | dark | white. Omit to keep
    /// alpha (most viewers show it on white, which hides light/white pixels).
    pub(crate) bg: Option<String>,
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
pub(crate) struct DocCritique {
    pub(crate) doc_id: String,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    pub(crate) region: Option<Vec<i32>>,
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
    /// generate (default) — synthesize a ramp/scheme · set — lock explicit
    /// `colors` on a doc · snap — snap a cel/doc to its palette · swap — recolour
    /// `from`→`to` · report — colour-usage tally · sync — broadcast one palette
    /// across a document set.
    pub(crate) op: Option<String>,
    // -- generate --
    /// `generate`: base colour [r,g,b(,a)] the ramp is built from (required).
    pub(crate) base: Option<Vec<i64>>,
    /// `generate`: mono | complementary | triadic | analogous | split | tetradic.
    pub(crate) scheme: Option<String>,
    /// `generate`: colours per ramp (default 5).
    pub(crate) count: Option<usize>,
    pub(crate) value_lo: Option<f32>,
    pub(crate) value_hi: Option<f32>,
    pub(crate) hue_shift: Option<f32>,
    /// `generate`: flat | arc | sat-in-shadow (default arc).
    pub(crate) sat_curve: Option<String>,
    pub(crate) anchor_midtone: Option<bool>,
    /// `generate`: store the flattened palette on this document id.
    pub(crate) set_doc: Option<String>,
    // -- doc-targeted ops (set/snap/swap/report) --
    /// Required for op=set|snap|swap|report (the document to act on).
    pub(crate) doc_id: Option<String>,
    pub(crate) layer: Option<usize>,
    pub(crate) frame: Option<usize>,
    /// `report`: [x0,y0,x1,y1] to restrict the tally; omit = whole canvas.
    pub(crate) region: Option<Vec<i32>>,
    /// `set`: palette swatches, each [r,g,b]/[r,g,b,a].
    pub(crate) colors: Option<Vec<Vec<i64>>>,
    /// `swap`: source colours to recolour (same length as `to`).
    pub(crate) from: Option<Vec<Vec<i64>>>,
    /// `swap`: replacement colours (same length as `from`).
    pub(crate) to: Option<Vec<Vec<i64>>>,
    /// `report`: max channel distance counting near-duplicates (default 8).
    pub(crate) dupe_threshold: Option<i32>,
    /// `snap`: override palette; `sync`: palette to broadcast. List of [r,g,b(,a)].
    pub(crate) palette: Option<Vec<Vec<i64>>>,
    /// `snap`: partial-alpha policy — preserve (default) | opaque | flatten.
    pub(crate) alpha: Option<String>,
    /// `snap`: alpha cutoff for alpha="opaque" (0..255, default 128).
    pub(crate) cutoff: Option<u8>,
    /// `snap`: backdrop [r,g,b] for alpha="flatten" (default opaque black).
    pub(crate) bg: Option<Vec<i64>>,
    /// `sync`: explicit target document ids (unioned with `prefix`).
    pub(crate) ids: Option<Vec<String>>,
    /// `sync`: select every document whose id starts with this.
    pub(crate) prefix: Option<String>,
    /// `sync`: copy the locked palette from this doc instead of passing `palette`.
    pub(crate) from_doc: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stringified_params_are_revived_except_text() {
        // Every showcase model hit this: a strict client stringifies params
        // the open flattened schema leaves untyped.
        let mut p = serde_json::from_value::<serde_json::Map<String, Value>>(serde_json::json!({
            "color": "[255, 0, 0]",
            "points": "[[1,2],[3,4]]",
            "dx": "2",
            "wrap": "true",
            "mode": "auto",
            "text": "[42]",
            "torn": "[1,"
        }))
        .unwrap();
        revive_params(&mut p);
        assert_eq!(p["color"], serde_json::json!([255, 0, 0]));
        assert_eq!(p["points"], serde_json::json!([[1, 2], [3, 4]]));
        assert_eq!(p["dx"], serde_json::json!(2));
        assert_eq!(p["wrap"], serde_json::json!(true));
        // Prose stays prose; malformed JSON stays a string for the op's error.
        assert_eq!(p["mode"], serde_json::json!("auto"));
        assert_eq!(p["text"], serde_json::json!("[42]"), "drawn text is prose");
        assert_eq!(p["torn"], serde_json::json!("[1,"));
    }

    #[test]
    fn doc_batch_ops_items_is_a_concrete_object_not_any() {
        // Regression: `Vec<serde_json::Value>` derives `items: true` (any),
        // which strict tool-call parsers (Gemini) reject. Pin it to an object.
        let schema = schemars::schema_for!(DocBatch);
        let items = &schema.as_value()["properties"]["ops"]["items"];
        assert_eq!(
            items["type"], "object",
            "ops items must be an object schema, got {items}"
        );
    }
}
