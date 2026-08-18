//! Tool parameter structs (`Deserialize` + `JsonSchema`) advertised by the
//! `#[tool]` router in `server`.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use atelier_core::document::{
    DitherAxis, DitherPattern, OpSide, TagDirection, draw_ops, fx_ops, operation_schema,
};
use atelier_core::raster::{Blend, SaturationCurve};
use atelier_studio::{
    AlphaMode, AnimAuditMode, AnimationFormat, CheckpointAction, CompareMode, DiffRender,
    DocumentId, DumpMode, ExportOp, FrameOp, LayerOp, LookBackground, LookMode, PaletteOp,
    PaletteScheme, ReferenceOp, RegionOp, SeamAxis, SheetMeta,
};

/// Preserve replay compatibility with journals produced by earlier 1.x clients
/// that stringified flattened operation values before the schemas were typed.
///
/// New clients receive concrete types from the registry-derived schemas. This
/// narrowly scoped shim remains idempotent for old recipes; drawn `text` stays
/// prose, and values that are not complete JSON remain strings for the strict
/// operation validator to reject.
pub(crate) fn revive_legacy_params(params: &mut serde_json::Map<String, Value>) {
    for (key, value) in params.iter_mut() {
        if key == "text" {
            continue;
        }
        if let Value::String(raw) = value {
            let candidate = raw.trim();
            let was_typed = candidate.starts_with('[')
                || candidate.starts_with('{')
                || candidate == "true"
                || candidate == "false"
                || candidate.parse::<f64>().is_ok();
            if was_typed && let Ok(parsed) = serde_json::from_str::<Value>(candidate) {
                *value = parsed;
            }
        }
    }
}

// --- library params --------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocNew {
    pub(crate) name: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocRef {
    pub(crate) doc_id: DocumentId,
}

#[derive(Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListDocs {
    /// Keep documents whose opaque id starts with this.
    pub(crate) prefix: Option<String>,
    /// Keep documents whose opaque id or display name contains this substring.
    pub(crate) contains: Option<String>,
    /// Exclusive opaque id returned as `next_cursor` by the previous page.
    pub(crate) cursor: Option<DocumentId>,
    /// Page size (default 50, maximum 100).
    #[schemars(range(min = 1, max = 100))]
    pub(crate) limit: Option<usize>,
}

// --- document params -------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocAddTag {
    pub(crate) doc_id: DocumentId,
    pub(crate) name: String,
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) direction: Option<TagDirection>,
}

// --- drawing params --------------------------------------------------------

macro_rules! op_name {
    ($name:ident, $valid:path, $label:literal) => {
        pub(crate) struct $name(String);

        impl $name {
            pub(crate) fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                if $valid().contains(&value.as_str()) {
                    Ok(Self(value))
                } else {
                    Err(serde::de::Error::custom(format!(
                        "unknown {} op '{}' — expected one of: {}",
                        $label,
                        value,
                        $valid().join(" | ")
                    )))
                }
            }
        }
    };
}

op_name!(DrawOpName, draw_ops, "draw");
op_name!(FxOpName, fx_ops, "fx");

#[derive(Deserialize)]
pub(crate) struct DocDraw {
    pub(crate) doc_id: DocumentId,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// One draw op: pencil | line | rect | ellipse | polyline | polygon | stroke
    /// | curve | stamp | fill | bucket | gradient | scatter | noise | text |
    /// fill_cel | clear_cel.
    pub(crate) op: DrawOpName,
    /// The op's own params, flattened alongside (e.g. for "rect": x0, y0, x1, y1,
    /// color, fill). Registry-backed ops also accept `opacity`, `blend_mode`,
    /// and `erase`.
    #[serde(flatten)]
    pub(crate) params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
pub(crate) struct DocFx {
    pub(crate) doc_id: DocumentId,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// One transform/effect op: blur | outline | drop_shadow | bevel | shade |
    /// form | dither | pixel_perfect | flip | shift | rotate | scale | symmetry |
    /// quantize | replace_color | adjust | gradient_map.
    pub(crate) op: FxOpName,
    /// The op's own params, flattened alongside. Every op also accepts
    /// `opacity`, `blend_mode`, and `erase`.
    #[serde(flatten)]
    pub(crate) params: serde_json::Map<String, serde_json::Value>,
}

fn single_op_tool_schema(generator: &mut SchemaGenerator, side: OpSide) -> Schema {
    let mut schema = operation_schema(side);
    let object = schema
        .as_object_mut()
        .expect("the core operation schema is an object");
    let properties = object
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("the core operation schema has properties");
    properties.insert(
        "doc_id".into(),
        <DocumentId as JsonSchema>::json_schema(generator).to_value(),
    );
    properties.insert(
        "layer".into(),
        serde_json::json!({"type": "integer", "minimum": 0}),
    );
    properties.insert(
        "frame".into(),
        serde_json::json!({"type": "integer", "minimum": 0}),
    );
    object.insert(
        "required".into(),
        serde_json::json!(["doc_id", "layer", "frame", "op"]),
    );
    schema
}

impl JsonSchema for DocDraw {
    fn schema_name() -> Cow<'static, str> {
        "DocDraw".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        single_op_tool_schema(generator, OpSide::Draw)
    }
}

impl JsonSchema for DocFx {
    fn schema_name() -> Cow<'static, str> {
        "DocFx".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        single_op_tool_schema(generator, OpSide::Fx)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocExport {
    pub(crate) doc_id: DocumentId,
    /// sheet | anim.
    pub(crate) op: ExportOp,
    pub(crate) out_path: String,
    /// Nearest-neighbour upscale (default 4).
    pub(crate) scale: Option<u32>,
    /// `sheet`: metadata dialect ("atelier" or "standard").
    pub(crate) meta: Option<SheetMeta>,
    /// `anim`: gif or apng.
    pub(crate) format: Option<AnimationFormat>,
    /// `anim`: optional animation tag.
    pub(crate) tag: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocLayer {
    pub(crate) doc_id: DocumentId,
    /// add (new layer on top) | set (visibility/opacity/blend of layer `index`) |
    /// move | insert | delete | rename | duplicate | merge_down.
    pub(crate) op: LayerOp,
    /// Target layer index (the layer for `set`/`move`/`delete`/…).
    pub(crate) index: Option<usize>,
    /// Destination index for `move`.
    pub(crate) to_index: Option<usize>,
    /// Layer name for `add`/`insert`/`rename`.
    pub(crate) name: Option<String>,
    /// Visibility for `set`.
    pub(crate) visible: Option<bool>,
    pub(crate) opacity: Option<u8>,
    pub(crate) blend: Option<Blend>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocFrame {
    pub(crate) doc_id: DocumentId,
    /// add (append) | duration (set frame timing) | delete | insert | duplicate |
    /// move.
    pub(crate) op: FrameOp,
    /// Target frame index (for duration/delete/insert/duplicate/move).
    pub(crate) frame: Option<usize>,
    /// For `add`: duplicate this frame's cels into the new frame.
    pub(crate) copy_from: Option<usize>,
    /// For add/duration: consecutive frame count (default 1, max 256).
    #[schemars(range(min = 1, max = 256))]
    pub(crate) count: Option<usize>,
    /// Destination index for `move`.
    pub(crate) to_index: Option<usize>,
    /// Frame duration in ms (for add/insert/duration; default 100).
    pub(crate) duration_ms: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocRegion {
    pub(crate) doc_id: DocumentId,
    /// clear | move.
    pub(crate) op: RegionOp,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// Inclusive source rectangle [x0,y0,x1,y1].
    pub(crate) rect: [i32; 4],
    /// For move, destination offset [dx,dy].
    pub(crate) offset: Option<[i32; 2]>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocRefOp {
    pub(crate) doc_id: DocumentId,
    /// set (attach/clear the comparison reference) | analyze (decompose the
    /// reference: background coverage, subject palette, silhouette grid) |
    /// compare (score a frame against the reference: silhouette IoU + per-cell
    /// ΔE) | diff (per-pixel signed error map vs the reference + worst pixels).
    pub(crate) op: ReferenceOp,
    /// `set`/`analyze`: reference image path (set: omit to clear; analyze: an
    /// external file instead of the stored reference).
    pub(crate) path: Option<String>,
    // -- compare/diff params --
    pub(crate) frame: Option<usize>,
    /// `analyze`: plan width.
    pub(crate) target_w: Option<u32>,
    /// `analyze`: subject-palette size (default 8).
    #[schemars(range(min = 2, max = 256))]
    pub(crate) colors: Option<usize>,
    /// `compare`: "side_by_side" (default) or "overlay" (reference ghosted under).
    pub(crate) mode: Option<CompareMode>,
    /// `compare`: ΔE grid divisions per axis (default 8, clamped 2..=16).
    pub(crate) cells: Option<u32>,
    // -- diff params --
    /// `diff`: worst individual pixels to list (default 20, clamped 1..=64).
    pub(crate) top: Option<usize>,
}

// --- canvas reader params --------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocDumpRegion {
    pub(crate) doc_id: DocumentId,
    pub(crate) frame: Option<usize>,
    /// Dump this layer's cel; omit to dump the flattened composite.
    pub(crate) layer: Option<usize>,
    /// [x0,y0,x1,y1] document pixels (inclusive). Omit = whole canvas. Area capped at 4096 px.
    pub(crate) region: Option<[i32; 4]>,
    /// "symbol" (A..Z a..z 0..9 per colour, `.`=transparent) or "hex".
    pub(crate) mode: Option<DumpMode>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocSilhouette {
    pub(crate) doc_id: DocumentId,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    /// Minimum alpha counted as opaque (default 1).
    pub(crate) alpha_threshold: Option<u8>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocComponents {
    pub(crate) doc_id: DocumentId,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    /// Pixel adjacency: 4 or 8 (default 8).
    #[schemars(schema_with = "connectivity_schema")]
    pub(crate) connectivity: Option<u8>,
    /// Only components of this exact [r,g,b]/[r,g,b,a]; omit = any opaque pixel.
    pub(crate) color: Option<Vec<i64>>,
    /// Components smaller than this are dropped from the list (default 1).
    #[schemars(range(min = 1))]
    pub(crate) min_area: Option<u32>,
}

fn connectivity_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type": "integer", "enum": [4, 8]})
}

// --- animation & tiling feedback params ------------------------------------

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocFrameDiff {
    pub(crate) doc_id: DocumentId,
    pub(crate) frame_a: usize,
    pub(crate) frame_b: usize,
    /// Diff this layer's cel; omit for the flattened composite.
    pub(crate) layer: Option<usize>,
    /// [x0,y0,x1,y1] document pixels; omit = whole canvas.
    pub(crate) region: Option<[i32; 4]>,
    /// Add a text grid (`.`unchanged `+`added `-`removed `~`recolored); area capped 4096 px.
    pub(crate) grid: Option<bool>,
    /// "none" or "overlay" (frame_b dimmed with changed pixels flagged).
    pub(crate) render: Option<DiffRender>,
    pub(crate) out_path: Option<String>,
    pub(crate) scale: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocSeamReport {
    pub(crate) doc_id: DocumentId,
    pub(crate) frame: Option<usize>,
    /// Test this layer's cel; omit for the flattened composite.
    pub(crate) layer: Option<usize>,
    /// "both", "horizontal" (left↔right) or "vertical" (top↔bottom).
    pub(crate) axis: Option<SeamAxis>,
    /// Max per-channel delta still counted as a matching edge (default 0).
    pub(crate) threshold: Option<i32>,
    /// Render a PNG with mismatched edge pixels highlighted red; returns its path.
    pub(crate) out_path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocAnimAudit {
    pub(crate) doc_id: DocumentId,
    /// Audit this tag's loop; omit to audit the whole timeline.
    pub(crate) tag: Option<String>,
    /// Use this layer's cel; omit for the flattened composite.
    pub(crate) layer: Option<usize>,
    /// "seam" (loop wrap diff), "spacing" (per-frame motion evenness),
    /// "arc" (trajectory shape) or "timing" (per-frame durations).
    pub(crate) mode: AnimAuditMode,
    /// [x0,y0,x1,y1] clips spacing/arc to one part (e.g. a swinging arm) so
    /// its motion is measurable over a static body.
    pub(crate) region: Option<[i32; 4]>,
}

// --- analysis and rendering params -----------------------------------------

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocLook {
    pub(crate) doc_id: DocumentId,
    pub(crate) frame: Option<usize>,
    pub(crate) scale: Option<u32>,
    /// Inclusive crop corners [x0,y0,x1,y1].
    pub(crate) region: Option<[i32; 4]>,
    /// render | value | bands | sat | hue | notan.
    pub(crate) mode: Option<LookMode>,
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
    pub(crate) bg: Option<LookBackground>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocCheckpoint {
    pub(crate) doc_id: DocumentId,
    /// save | list | restore | prune.
    pub(crate) action: CheckpointAction,
    /// Optional save label. The studio enforces a 4096-byte UTF-8 limit.
    #[schemars(length(max = 4096))]
    pub(crate) label: Option<String>,
    pub(crate) checkpoint_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocCritique {
    pub(crate) doc_id: DocumentId,
    pub(crate) frame: Option<usize>,
    pub(crate) layer: Option<usize>,
    pub(crate) region: Option<[i32; 4]>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocDitherRamp {
    pub(crate) doc_id: DocumentId,
    pub(crate) layer: usize,
    pub(crate) frame: usize,
    /// The ramp to dither across (>= 2 colours), as [[r,g,b],...].
    #[schemars(length(min = 2, max = 256))]
    pub(crate) ramp: Vec<Vec<i64>>,
    pub(crate) region: Option<[i32; 4]>,
    /// h | v | radial.
    pub(crate) axis: Option<DitherAxis>,
    /// bayer2 | bayer4 | bayer8 | checker | ign.
    pub(crate) pattern: Option<DitherPattern>,
    pub(crate) only_existing: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocContactSheet {
    pub(crate) doc_id: DocumentId,
    pub(crate) scale: Option<u32>,
    pub(crate) cols: Option<usize>,
    /// Ghost each cell's PREVIOUS frame under it at 35% alpha — per-pair
    /// onion skinning, the closest a still image gets to showing motion.
    pub(crate) onion: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocPalette {
    /// generate (default) — synthesize a ramp/scheme · set — lock explicit
    /// `colors` on a doc · snap — snap a cel/doc to its palette · swap — recolour
    /// `from`→`to` · report — colour-usage tally.
    pub(crate) op: Option<PaletteOp>,
    // -- generate --
    /// `generate`: base colour [r,g,b(,a)] the ramp is built from (required).
    pub(crate) base: Option<Vec<i64>>,
    /// `generate`: mono | complementary | triadic | analogous | split | tetradic.
    pub(crate) scheme: Option<PaletteScheme>,
    /// `generate`: colours per ramp (default 5, maximum 256; multi-hue schemes require 2+).
    #[schemars(range(min = 1, max = 256))]
    pub(crate) count: Option<usize>,
    pub(crate) value_lo: Option<f32>,
    pub(crate) value_hi: Option<f32>,
    pub(crate) hue_shift: Option<f32>,
    /// `generate`: flat | arc | sat-in-shadow (default arc).
    pub(crate) sat_curve: Option<SaturationCurve>,
    pub(crate) anchor_midtone: Option<bool>,
    /// `generate`: store the flattened palette on this document id.
    pub(crate) set_doc: Option<DocumentId>,
    // -- doc-targeted ops (set/snap/swap/report) --
    /// Required for op=set|snap|swap|report (the document to act on).
    pub(crate) doc_id: Option<DocumentId>,
    pub(crate) layer: Option<usize>,
    pub(crate) frame: Option<usize>,
    /// `report`: [x0,y0,x1,y1] to restrict the tally; omit = whole canvas.
    pub(crate) region: Option<[i32; 4]>,
    /// `set`: palette swatches, each [r,g,b]/[r,g,b,a].
    #[schemars(length(max = 256))]
    pub(crate) colors: Option<Vec<Vec<i64>>>,
    /// `swap`: source colours to recolour (same length as `to`).
    #[schemars(length(min = 1, max = 256))]
    pub(crate) from: Option<Vec<Vec<i64>>>,
    /// `swap`: replacement colours (same length as `from`).
    #[schemars(length(min = 1, max = 256))]
    pub(crate) to: Option<Vec<Vec<i64>>>,
    /// `report`: max channel distance counting near-duplicates (default 8).
    pub(crate) dupe_threshold: Option<i32>,
    /// `snap`: override palette. List of [r,g,b(,a)].
    #[schemars(length(min = 1, max = 256))]
    pub(crate) palette: Option<Vec<Vec<i64>>>,
    /// `snap`: partial-alpha policy — preserve (default) | opaque | flatten.
    pub(crate) alpha: Option<AlphaMode>,
    /// `snap`: alpha cutoff for alpha="opaque" (0..255, default 128).
    pub(crate) cutoff: Option<u8>,
    /// `snap`: backdrop [r,g,b] for alpha="flatten" (default opaque black).
    pub(crate) bg: Option<Vec<i64>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocPaintGrid {
    pub(crate) doc_id: DocumentId,
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
    fn legacy_stringified_params_are_revived_except_text() {
        let mut params =
            serde_json::from_value::<serde_json::Map<String, Value>>(serde_json::json!({
                "color": "[255, 0, 0]",
                "points": "[[1,2],[3,4]]",
                "dx": "2",
                "wrap": "true",
                "mode": "auto",
                "text": "[42]",
                "torn": "[1,"
            }))
            .unwrap();
        revive_legacy_params(&mut params);
        assert_eq!(params["color"], serde_json::json!([255, 0, 0]));
        assert_eq!(params["points"], serde_json::json!([[1, 2], [3, 4]]));
        assert_eq!(params["dx"], serde_json::json!(2));
        assert_eq!(params["wrap"], serde_json::json!(true));
        assert_eq!(params["mode"], serde_json::json!("auto"));
        assert_eq!(params["text"], serde_json::json!("[42]"));
        assert_eq!(params["torn"], serde_json::json!("[1,"));
    }

    #[test]
    fn typed_tool_params_reject_removed_or_misspelled_fields() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert!(
            serde_json::from_value::<DocNew>(serde_json::json!({
                "name": "x",
                "width": 8,
                "height": 8,
                "doc_id": "old"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DocPalette>(serde_json::json!({
                "op": "set",
                "doc_id": id,
                "colors": [[0, 0, 0]],
                "ids": ["old"]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DocExport>(serde_json::json!({
                "doc_id": id,
                "op": "sheet",
                "out_path": "x.png",
                "params": {}
            }))
            .is_err()
        );
    }

    #[test]
    fn closed_control_values_and_document_ids_are_typed() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert!(
            serde_json::from_value::<DocLayer>(serde_json::json!({
                "doc_id": id,
                "op": "add",
                "blend": "multiply"
            }))
            .is_ok()
        );
        for bad in [
            serde_json::json!({"doc_id": id, "op": "append"}),
            serde_json::json!({"doc_id": id, "op": "add", "blend": "source-over"}),
            serde_json::json!({"doc_id": "hero", "op": "add"}),
        ] {
            assert!(
                serde_json::from_value::<DocLayer>(bad).is_err(),
                "accepted a non-contract control value"
            );
        }
        assert!(
            serde_json::from_value::<DocDraw>(serde_json::json!({
                "doc_id": id,
                "layer": 0,
                "frame": 0,
                "op": "rect",
                "x0": 0
            }))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<DocDraw>(serde_json::json!({
                "doc_id": id,
                "layer": 0,
                "frame": 0,
                "op": "box_iso"
            }))
            .is_err(),
            "accepted a removed draw op"
        );
        assert!(
            serde_json::from_value::<DocFx>(serde_json::json!({
                "doc_id": id,
                "layer": 0,
                "frame": 0,
                "op": "glow"
            }))
            .is_err(),
            "accepted the removed glow operation through doc_fx"
        );
        // Undocumented aliases are not compatibility APIs.
        for alias in ["grayscale", "saturation"] {
            assert!(
                serde_json::from_value::<DocLook>(serde_json::json!({
                    "doc_id": id,
                    "mode": alias
                }))
                .is_err()
            );
        }
        // A region is exactly four coordinates in both deserialization and schema.
        assert!(
            serde_json::from_value::<DocLook>(serde_json::json!({
                "doc_id": id,
                "region": [0, 0, 1]
            }))
            .is_err()
        );
    }

    #[test]
    fn schemas_advertise_closed_values_instead_of_free_strings() {
        let layer = schemars::schema_for!(DocLayer);
        let layer = layer.as_value();
        assert_eq!(layer["properties"]["op"]["$ref"], "#/$defs/LayerOp");
        assert_eq!(
            layer["$defs"]["LayerOp"]["enum"],
            serde_json::json!([
                "add",
                "set",
                "move",
                "insert",
                "delete",
                "rename",
                "duplicate",
                "merge_down"
            ])
        );
        assert_eq!(layer["properties"]["doc_id"]["$ref"], "#/$defs/DocumentId");
        assert_eq!(layer["$defs"]["DocumentId"]["type"], "string");
        assert_eq!(layer["$defs"]["DocumentId"]["format"], "uuid");

        let draw = schemars::schema_for!(DocDraw);
        let draw = draw.as_value();
        assert_eq!(draw["additionalProperties"], false);
        assert_eq!(draw["properties"]["doc_id"]["format"], "uuid");
        assert_eq!(parameter_schema(draw, "color")["$ref"], "#/$defs/c");
        assert_eq!(parameter_schema(draw, "opacity")["maximum"], 255);
        for op in draw_ops() {
            assert!(
                op_branch(draw, op).is_some(),
                "missing draw schema for {op}"
            );
        }
        let rect = op_branch(draw, "rect").unwrap();
        assert_eq!(
            rect["required"],
            serde_json::json!(["x0", "y0", "x1", "y1", "color"])
        );
        assert_eq!(draw["properties"]["octaves"]["minimum"], 1);
        assert_eq!(
            draw["properties"]["octaves"]["maximum"],
            atelier_core::document::MAX_NOISE_OCTAVES
        );

        let fx = schemars::schema_for!(DocFx);
        let fx = fx.as_value();
        for op in fx_ops() {
            assert!(op_branch(fx, op).is_some(), "missing fx schema for {op}");
        }
        assert_eq!(fx["properties"]["max_colors"]["minimum"], 1);
        assert_eq!(
            fx["properties"]["max_colors"]["maximum"],
            atelier_core::document::MAX_QUANTIZE_COLORS
        );

        let checkpoint = schemars::schema_for!(DocCheckpoint);
        let checkpoint = checkpoint.as_value();
        assert_eq!(checkpoint["properties"]["label"]["maxLength"], 4096);

        let frame = schemars::schema_for!(DocFrame);
        let frame = frame.as_value();
        assert_eq!(frame["properties"]["count"]["minimum"], 1);
        assert_eq!(frame["properties"]["count"]["maximum"], 256);
    }

    fn op_branch<'a>(schema: &'a Value, op: &str) -> Option<&'a Value> {
        schema["oneOf"].as_array()?.iter().find(|branch| {
            let selector = &branch["properties"]["op"];
            selector["const"].as_str() == Some(op)
                || selector["enum"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name.as_str() == Some(op)))
        })
    }

    fn parameter_schema<'a>(schema: &'a Value, key: &str) -> &'a Value {
        schema["properties"]
            .get(key)
            .unwrap_or_else(|| panic!("schema is missing typed parameter {key}"))
    }
}
