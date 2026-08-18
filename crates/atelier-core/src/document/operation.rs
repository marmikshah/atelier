//! JSON drawing operations: dispatch, key registry, and strict validation.

use image::Rgba;
use schemars::Schema;
use serde_json::{Map, Value, json};

use crate::raster;

use super::{
    AlphaSnap, Document, MAX_GRADIENT_STOPS, MAX_NOISE_OCTAVES, MAX_PALETTE_COLORS,
    MAX_QUANTIZE_COLORS,
};

impl Document {
    /// Apply one drawing operation described by a JSON object
    /// `{"op": "...", ...}`.
    ///
    /// Optional per-op `"opacity"` (0..255) and `"blend_mode"` (a layer blend
    /// name) composite the op's result instead of overwriting: the op is run,
    /// then the pixels it changed are re-composited over the pre-op cel with the
    /// given opacity/mode (snapshot-diff, so any op gains blend/opacity).
    /// `"erase": true` turns the op into an ERASER instead: every pixel the op
    /// touched becomes transparent — any shape (pencil/line/ellipse/fill/…) can
    /// punch a hole, which no colour trick could do (drawing `[0,0,0,0]` is a
    /// no-op under source-over).
    pub fn apply_op(&mut self, layer: usize, frame: usize, op: &Value) -> Result<(), String> {
        // `Document` is a public core API, not merely an implementation detail
        // behind Studio. Enforce the same strict contract here so direct users
        // cannot bypass validation and reach the lossy parsing helpers below.
        validate_op(op)?;
        let opacity = op.get("opacity").and_then(|v| v.as_u64()).map(|v| v as u8);
        let mode = match op.get("blend_mode") {
            None => None,
            Some(value) => {
                let name = value
                    .as_str()
                    .ok_or_else(|| format!("blend_mode must be a string, got {value}"))?;
                Some(name.parse::<raster::Blend>()?)
            }
        };
        let erase = op.get("erase").and_then(|v| v.as_bool()).unwrap_or(false);
        if !erase && opacity.is_none() && mode.is_none() {
            return self.apply_op_raw(layer, frame, op);
        }
        let before = self.cel_canvas(layer, frame)?.clone();
        self.apply_op_raw(layer, frame, op)?;
        if erase {
            // The op is only a stencil: everything it marked goes transparent.
            let img = self.cel_canvas(layer, frame)?;
            for y in 0..img.height() {
                for x in 0..img.width() {
                    if img.get_pixel(x, y).0 != before.get_pixel(x, y).0 {
                        img.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                    }
                }
            }
            return Ok(());
        }
        let of = opacity.unwrap_or(255) as f32 / 255.0;
        let m = mode.unwrap_or(raster::Blend::Normal);
        let img = self.cel_canvas(layer, frame)?;
        for y in 0..img.height() {
            for x in 0..img.width() {
                let after = img.get_pixel(x, y).0;
                let base = before.get_pixel(x, y).0;
                if after != base {
                    img.put_pixel(x, y, Rgba(raster::composite_px(base, after, of, m)));
                }
            }
        }
        Ok(())
    }

    /// Dispatch one op through the [`OPS`] table, then run the shared post-op
    /// discipline (on-palette re-snap for the continuous-tone FX).
    pub(super) fn apply_op_raw(
        &mut self,
        layer: usize,
        frame: usize,
        op: &Value,
    ) -> Result<(), String> {
        let name = op
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or("operation missing 'op'")?;
        let spec = find_op(name).ok_or_else(|| format!("unknown operation '{name}'"))?;
        (spec.run)(self, layer, frame, op)?;
        // Continuous-tone FX push a locked palette into hundreds of colours; re-snap
        // onto it by default (as gradient does) so the result stays crisp
        // pixel art. Opt out per op with `snap:false`. Hard-edged / already-on-palette
        // ops (outline, dither, pixel_perfect, quantize, adjust) are excluded.
        if matches!(name, "blur" | "drop_shadow" | "bevel" | "form" | "shade")
            && gb(op, "snap", true)
        {
            self.snap_cel_to_own_palette(layer, frame, AlphaSnap::Preserve)?;
        }
        Ok(())
    }
}

// -- the op registry ----------------------------------------------------------
//
// ONE table drives everything: `apply_op_raw` dispatches on it, the strict
// validator reads its key lists, and the doc_draw/doc_fx vocabularies are
// filtered from it. It used to be three hand-synced lists (dispatch match,
// key table, draw/fx partition) with two tests existing only to catch drift —
// a new op is now exactly one entry here.

/// Which single-operation tool vocabulary an operation belongs to.
/// `doc_draw` adds new marks and `doc_fx` reworks existing pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpSide {
    Draw,
    Fx,
}

/// One operation: its name, JSON schema (required/optional keys), side, and
/// executor. Every op additionally accepts the compositing wrapper keys
/// `opacity`/`blend_mode`/`erase` (see `apply_op`).
pub(crate) struct OpSpec {
    pub(crate) name: &'static str,
    pub(crate) required: &'static [&'static str],
    pub(crate) optional: &'static [&'static str],
    pub(crate) side: OpSide,
    pub(crate) run: fn(&mut Document, usize, usize, &Value) -> Result<(), String>,
}

pub(crate) static OPS: &[OpSpec] = &[
    // -- draw: add new marks --
    OpSpec {
        name: "pencil",
        required: &["points", "color"],
        optional: &["size"],
        side: OpSide::Draw,
        run: op_pencil,
    },
    OpSpec {
        name: "line",
        required: &["x0", "y0", "x1", "y1", "color"],
        optional: &["size"],
        side: OpSide::Draw,
        run: op_line,
    },
    OpSpec {
        name: "rect",
        required: &["x0", "y0", "x1", "y1", "color"],
        optional: &["fill", "size"],
        side: OpSide::Draw,
        run: op_rect,
    },
    OpSpec {
        name: "ellipse",
        required: &["cx", "cy", "rx", "ry", "color"],
        optional: &["fill"],
        side: OpSide::Draw,
        run: op_ellipse,
    },
    OpSpec {
        name: "polyline",
        required: &["points", "color"],
        optional: &["size", "closed"],
        side: OpSide::Draw,
        run: op_polyline,
    },
    OpSpec {
        name: "polygon",
        required: &["points", "color"],
        optional: &["fill"],
        side: OpSide::Draw,
        run: op_polygon,
    },
    OpSpec {
        name: "stroke",
        required: &["points", "color"],
        optional: &["width", "aa", "snap"],
        side: OpSide::Draw,
        run: op_stroke,
    },
    OpSpec {
        name: "curve",
        required: &["points", "color"],
        optional: &["width", "aa", "snap"],
        side: OpSide::Draw,
        run: op_curve,
    },
    OpSpec {
        name: "stamp",
        required: &["points", "tip"],
        optional: &["colorize"],
        side: OpSide::Draw,
        run: op_stamp,
    },
    OpSpec {
        name: "fill",
        required: &["x", "y", "color"],
        optional: &["tolerance"],
        side: OpSide::Draw,
        run: op_fill,
    },
    OpSpec {
        name: "bucket",
        required: &["x", "y", "color"],
        optional: &["tolerance"],
        side: OpSide::Draw,
        run: op_fill,
    },
    OpSpec {
        name: "gradient",
        required: &["stops"],
        optional: &[
            "kind", "x0", "y0", "x1", "y1", "dither", "seed", "region", "blend", "snap",
        ],
        side: OpSide::Draw,
        run: op_gradient,
    },
    OpSpec {
        name: "scatter",
        required: &["colors", "x0", "y0", "x1", "y1"],
        optional: &["density", "seed", "size"],
        side: OpSide::Draw,
        run: op_scatter,
    },
    OpSpec {
        name: "noise",
        required: &["stops", "x0", "y0", "x1", "y1"],
        optional: &["kind", "scale", "octaves", "seed", "blend"],
        side: OpSide::Draw,
        run: op_noise,
    },
    OpSpec {
        name: "text",
        required: &["x", "y", "text", "color"],
        optional: &["size"],
        side: OpSide::Draw,
        run: op_text,
    },
    OpSpec {
        name: "fill_cel",
        required: &["color"],
        optional: &[],
        side: OpSide::Draw,
        run: op_fill_cel,
    },
    OpSpec {
        name: "clear_cel",
        required: &[],
        optional: &[],
        side: OpSide::Draw,
        run: op_clear_cel,
    },
    // -- fx: rework existing pixels --
    OpSpec {
        name: "blur",
        required: &["radius"],
        optional: &["region", "snap"],
        side: OpSide::Fx,
        run: op_blur,
    },
    OpSpec {
        name: "outline",
        required: &["color"],
        optional: &["aa"],
        side: OpSide::Fx,
        run: op_outline,
    },
    OpSpec {
        name: "drop_shadow",
        required: &["color"],
        optional: &["dx", "dy", "blur", "snap", "shadow_opacity"],
        side: OpSide::Fx,
        run: op_drop_shadow,
    },
    OpSpec {
        name: "bevel",
        required: &["light", "dark"],
        optional: &["depth", "snap"],
        side: OpSide::Fx,
        run: op_bevel,
    },
    OpSpec {
        name: "shade",
        required: &[],
        optional: &["light_dir", "steps", "region", "mode", "ramp", "snap"],
        side: OpSide::Fx,
        run: op_shade,
    },
    OpSpec {
        name: "form",
        required: &[],
        optional: &["form", "light_dir", "region", "ramp", "strength", "snap"],
        side: OpSide::Fx,
        run: op_form,
    },
    OpSpec {
        name: "dither",
        required: &["color_a", "color_b"],
        optional: &["region", "pattern", "density", "only_existing"],
        side: OpSide::Fx,
        run: op_dither,
    },
    OpSpec {
        name: "pixel_perfect",
        required: &[],
        optional: &["region", "color"],
        side: OpSide::Fx,
        run: op_pixel_perfect,
    },
    OpSpec {
        name: "flip",
        required: &[],
        optional: &["horizontal"],
        side: OpSide::Fx,
        run: op_flip,
    },
    OpSpec {
        name: "shift",
        required: &[],
        optional: &["dx", "dy", "wrap"],
        side: OpSide::Fx,
        run: op_shift,
    },
    OpSpec {
        name: "rotate",
        required: &[],
        optional: &["turns"],
        side: OpSide::Fx,
        run: op_rotate,
    },
    OpSpec {
        name: "scale",
        required: &["w", "h"],
        optional: &["method"],
        side: OpSide::Fx,
        run: op_scale,
    },
    OpSpec {
        name: "symmetry",
        required: &[],
        optional: &["vertical", "horizontal", "keep_left", "keep_top"],
        side: OpSide::Fx,
        run: op_symmetry,
    },
    OpSpec {
        name: "quantize",
        required: &["colors"],
        optional: &["max_colors"],
        side: OpSide::Fx,
        run: op_quantize,
    },
    OpSpec {
        name: "replace_color",
        required: &["from", "to"],
        optional: &["tolerance"],
        side: OpSide::Fx,
        run: op_replace_color,
    },
    OpSpec {
        name: "adjust",
        required: &[],
        optional: &["region", "hue", "sat", "lum"],
        side: OpSide::Fx,
        run: op_adjust,
    },
    OpSpec {
        name: "gradient_map",
        required: &["stops"],
        optional: &["region"],
        side: OpSide::Fx,
        run: op_gradient_map,
    },
];

fn find_op(name: &str) -> Option<&'static OpSpec> {
    OPS.iter().find(|s| s.name == name)
}

/// The op names of one side, in table order — the doc_draw/doc_fx vocabularies.
fn side_ops(side: OpSide) -> &'static [&'static str] {
    static DRAW: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    static FX: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    let cell = match side {
        OpSide::Draw => &DRAW,
        OpSide::Fx => &FX,
    };
    cell.get_or_init(|| {
        OPS.iter()
            .filter(|s| s.side == side)
            .map(|s| s.name)
            .collect()
    })
}

/// The doc_draw vocabulary (`fill_cel`/`clear_cel` are draw-side).
pub fn draw_ops() -> &'static [&'static str] {
    side_ops(OpSide::Draw)
}

/// The `doc_fx` vocabulary.
pub fn fx_ops() -> &'static [&'static str] {
    side_ops(OpSide::Fx)
}

/// Compact JSON Schema for one side of the single-operation API.
///
/// The same `OPS` registry entries that dispatch and validate operations supply the
/// discriminator branches and required keys. Parameter types are advertised
/// once in the top-level property union, keeping both tool schemas small
/// enough to ship on every MCP session while still preventing clients from
/// guessing that colours, points, numbers, and booleans are strings.
pub fn operation_schema(side: OpSide) -> Schema {
    let specs: Vec<&OpSpec> = OPS.iter().filter(|spec| spec.side == side).collect();
    let mut properties = Map::new();
    properties.insert("op".into(), json!({"type": "string"}));
    // The operation validator applies the closed Blend vocabulary. Keep the
    // session schema compact here; the important client-side distinction is
    // that this control is a string, not an arbitrary object or number.
    properties.insert("blend_mode".into(), json!({"type": "string"}));

    // A key can intentionally have different shapes on different operations
    // (`horizontal` is a boolean for flip and an integer axis for symmetry;
    // stroke points optionally carry width). Preserve that union instead of
    // picking whichever operation happens to appear first.
    let mut variants: std::collections::BTreeMap<&str, Vec<Value>> =
        std::collections::BTreeMap::new();
    variants.insert(
        "opacity",
        vec![json!({"type": "integer", "minimum": 0, "maximum": 255})],
    );
    variants.insert("erase", vec![json!({"type": "boolean"})]);
    for spec in &specs {
        for key in spec.required.iter().chain(spec.optional) {
            let schema = operation_param_schema(spec.name, key);
            let choices = variants.entry(key).or_default();
            if !choices.contains(&schema) {
                choices.push(schema);
            }
        }
    }
    for (key, mut choices) in variants {
        let schema = if choices.len() == 1 {
            choices.pop().expect("one schema variant")
        } else {
            json!({"anyOf": choices})
        };
        // Keep each field explicit. Some MCP clients ignore patternProperties,
        // which made valid operation arguments invisible and caused avoidable
        // schema-correction calls.
        properties.insert(key.into(), schema);
    }

    let mut branch_groups: Vec<(&[&str], Vec<&str>)> = Vec::new();
    for spec in specs {
        if let Some((_, names)) = branch_groups
            .iter_mut()
            .find(|(required, _)| *required == spec.required)
        {
            names.push(spec.name);
        } else {
            branch_groups.push((spec.required, vec![spec.name]));
        }
    }
    let branches: Vec<Value> = branch_groups
        .into_iter()
        .map(|(required, names)| {
            let selector = if names.len() == 1 {
                json!({"const": names[0]})
            } else {
                json!({"enum": names})
            };
            let mut branch = json!({"properties": {"op": selector}});
            if !required.is_empty() {
                branch.as_object_mut().expect("branch object").insert(
                    "required".into(),
                    Value::Array(
                        required
                            .iter()
                            .map(|key| Value::String((*key).into()))
                            .collect(),
                    ),
                );
            }
            branch
        })
        .collect();
    json!({
        "type": "object",
        "$defs": {
            "c": {
                "type": "array",
                "items": {"type": "integer", "minimum": 0, "maximum": 255},
                "minItems": 3,
                "maxItems": 4
            },
            "cs": {
                "type": "array",
                "items": {"$ref": "#/$defs/c"},
                "maxItems": MAX_PALETTE_COLORS
            },
            "i": {"type": "integer"},
            "n": {"type": "number"},
            "b": {"type": "boolean"},
            "s": {"type": "string"}
        },
        "properties": properties,
        "oneOf": branches,
        "required": ["op"],
        "additionalProperties": false
    })
    .try_into()
    .expect("operation schema is an object")
}

fn operation_param_schema(op: &str, key: &str) -> Value {
    let integer = || json!({"$ref": "#/$defs/i"});
    let number = || json!({"$ref": "#/$defs/n"});
    let boolean = || json!({"$ref": "#/$defs/b"});
    let string = || json!({"$ref": "#/$defs/s"});
    let color = || json!({"$ref": "#/$defs/c"});
    let colors = || json!({"$ref": "#/$defs/cs"});
    match key {
        "color" | "light" | "dark" | "color_a" | "color_b" | "from" | "to" | "colorize" => color(),
        "colors" | "ramp" => colors(),
        // Across the vocabulary points are integer pairs, numeric pairs, or
        // numeric triples (stroke width). The operation validator enforces the
        // narrower per-discriminator shape.
        "points" => json!({
            "type": "array",
            "items": {
                "type": "array",
                "items": {"type": "number"},
                "minItems": 2,
                "maxItems": 3
            }
        }),
        "region" => json!({
            "type": "array",
            "items": {"type": "integer"},
            "minItems": 4,
            "maxItems": 4
        }),
        "stops" => json!({
            "type": "array",
            "minItems": 1,
            "maxItems": MAX_GRADIENT_STOPS,
            "items": {
                "type": "object",
                "properties": {"pos": {"type": "number"}, "color": color()},
                "required": ["pos", "color"],
                "additionalProperties": false
            }
        }),
        "tip" => json!({
            "type": "object",
            "properties": {
                "w": {"type": "integer", "minimum": 1},
                "h": {"type": "integer", "minimum": 1},
                "pixels": colors()
            },
            "required": ["w", "h", "pixels"],
            "additionalProperties": false
        }),
        "opacity" | "shadow_opacity" => {
            json!({"type": "integer", "minimum": 0, "maximum": 255})
        }
        "octaves" => json!({
            "type": "integer",
            "minimum": 1,
            "maximum": MAX_NOISE_OCTAVES
        }),
        "turns" => json!({"type": "integer", "minimum": 0, "maximum": 255}),
        "w" | "h" => json!({"type": "integer", "minimum": 1}),
        "max_colors" => json!({
            "type": "integer",
            "minimum": 1,
            "maximum": MAX_QUANTIZE_COLORS
        }),
        "seed" => json!({"type": "integer", "minimum": 0}),
        "horizontal" if op == "symmetry" => integer(),
        "size" => json!({"type": "integer", "minimum": 1}),
        "x" | "y" | "x0" | "y0" | "x1" | "y1" | "cx" | "cy" | "rx" | "ry" | "tolerance"
        | "radius" | "dx" | "dy" | "blur" | "depth" | "steps" | "vertical" => integer(),
        "width" | "density" | "strength" | "hue" | "sat" | "lum" | "scale" => number(),
        "fill" | "closed" | "aa" | "snap" | "blend" | "only_existing" | "wrap" | "keep_left"
        | "keep_top" | "horizontal" => boolean(),
        "kind" | "dither" | "text" | "method" | "light_dir" | "mode" | "form" | "pattern" => {
            string()
        }
        _ => panic!("operation registry key '{key}' has no JSON Schema type"),
    }
}

// -- op executors (ported 1:1 from the old dispatch match) --------------------

fn gi(op: &Value, k: &str, d: i64) -> i32 {
    op.get(k).and_then(|v| v.as_i64()).unwrap_or(d) as i32
}

fn gb(op: &Value, k: &str, d: bool) -> bool {
    op.get(k).and_then(|v| v.as_bool()).unwrap_or(d)
}

fn col(op: &Value, k: &str) -> [u8; 4] {
    rgba_val(op.get(k))
}

fn op_pencil(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.pencil(
        l,
        f,
        &points_val(op.get("points")),
        col(op, "color"),
        gi(op, "size", 1),
    )
}

fn op_line(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.line(
        l,
        f,
        gi(op, "x0", 0),
        gi(op, "y0", 0),
        gi(op, "x1", 0),
        gi(op, "y1", 0),
        col(op, "color"),
        gi(op, "size", 1),
    )
}

fn op_rect(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.rect(
        l,
        f,
        gi(op, "x0", 0),
        gi(op, "y0", 0),
        gi(op, "x1", 0),
        gi(op, "y1", 0),
        col(op, "color"),
        gb(op, "fill", false),
        gi(op, "size", 1),
    )
}

fn op_ellipse(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.ellipse(
        l,
        f,
        gi(op, "cx", 0),
        gi(op, "cy", 0),
        gi(op, "rx", 1),
        gi(op, "ry", 1),
        col(op, "color"),
        gb(op, "fill", false),
    )
}

fn op_polyline(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.polyline(
        l,
        f,
        &points_val(op.get("points")),
        col(op, "color"),
        gi(op, "size", 1),
        gb(op, "closed", false),
    )
}

fn op_polygon(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.polygon(
        l,
        f,
        &points_val(op.get("points")),
        col(op, "color"),
        gb(op, "fill", true),
    )
}

fn op_stroke(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    let w = op.get("width").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32;
    d.stroke_f(
        l,
        f,
        &points3f_val(op.get("points"), w),
        col(op, "color"),
        gb(op, "aa", true),
        gb(op, "snap", true),
    )
}

fn op_curve(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.curve(
        l,
        f,
        &points2f_val(op.get("points")),
        col(op, "color"),
        op.get("width").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32,
        gb(op, "aa", true),
        gb(op, "snap", true),
    )
}

fn op_stamp(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    // The tip travels as plain JSON (`{w, h, pixels: [[r,g,b,a], ...]}`) so a
    // stamped call stays self-contained for journaling and replay — no
    // base64, no external file.
    let tip = op.get("tip").ok_or("stamp needs 'tip'")?;
    let w = tip.get("w").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let h = tip.get("h").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let px = colors_val(tip.get("pixels"));
    if w == 0 || h == 0 || px.len() != (w * h) as usize {
        return Err(format!(
            "stamp tip must have exactly w*h pixels ({w}×{h} = {}, got {})",
            w * h,
            px.len()
        ));
    }
    let mut buf = Vec::with_capacity(px.len() * 4);
    for c in px {
        buf.extend_from_slice(&c);
    }
    let img = image::RgbaImage::from_raw(w, h, buf)
        .ok_or_else(|| format!("stamp tip {w}×{h} does not fit its pixel buffer"))?;
    let colorize = op.get("colorize").map(|_| col(op, "colorize"));
    d.stamp_tip(l, f, &points_val(op.get("points")), &img, colorize)
        .map(|_| ())
}

fn op_rotate(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.rotate_cel(l, f, gi(op, "turns", 1) as u8)
}

fn op_scale(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.scale_cel(
        l,
        f,
        gi(op, "w", 0) as u32,
        gi(op, "h", 0) as u32,
        op.get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("nearest"),
    )
}

fn op_fill(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.bucket_fill(
        l,
        f,
        gi(op, "x", 0),
        gi(op, "y", 0),
        col(op, "color"),
        gi(op, "tolerance", 0),
    )
}

fn op_gradient(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.gradient(
        l,
        f,
        op.get("kind").and_then(|v| v.as_str()).unwrap_or("linear"),
        gi(op, "x0", 0),
        gi(op, "y0", 0),
        gi(op, "x1", 0),
        gi(op, "y1", 0),
        stops_val(op.get("stops")),
        op.get("dither").and_then(|v| v.as_str()).unwrap_or("none"),
        op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
        region_val(op.get("region")),
        gb(op, "blend", true),
    )?;
    // Re-snap on-palette by default when a palette is locked.
    if gb(op, "snap", true) {
        d.snap_cel_to_own_palette(l, f, AlphaSnap::Preserve)?;
    }
    Ok(())
}

fn op_scatter(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.scatter(
        l,
        f,
        gi(op, "x0", 0),
        gi(op, "y0", 0),
        gi(op, "x1", 0),
        gi(op, "y1", 0),
        &colors_val(op.get("colors")),
        op.get("density").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32,
        op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
        gi(op, "size", 1),
    )
}

fn op_noise(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.noise(
        l,
        f,
        op.get("kind").and_then(|v| v.as_str()).unwrap_or("cloud"),
        gi(op, "x0", 0),
        gi(op, "y0", 0),
        gi(op, "x1", 0),
        gi(op, "y1", 0),
        op.get("scale").and_then(|v| v.as_f64()).unwrap_or(8.0) as f32,
        op.get("octaves").and_then(|v| v.as_u64()).unwrap_or(4) as u32,
        op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
        stops_val(op.get("stops")),
        gb(op, "blend", false),
    )
}

fn op_text(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.text(
        l,
        f,
        gi(op, "x", 0),
        gi(op, "y", 0),
        op.get("text").and_then(|v| v.as_str()).unwrap_or(""),
        col(op, "color"),
        gi(op, "size", 1),
    )
    .map(|_| ())
}

fn op_fill_cel(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.fill_cel(l, f, col(op, "color"))
}

fn op_clear_cel(d: &mut Document, l: usize, f: usize, _op: &Value) -> Result<(), String> {
    d.clear_cel(l, f)
}

fn op_blur(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.blur(l, f, gi(op, "radius", 1), region_val(op.get("region")))
}

fn op_outline(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.outline_cel(l, f, col(op, "color"), gb(op, "aa", false))
}

fn op_drop_shadow(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    // `shadow_opacity`, not `opacity`: the plain key is the operation-wide
    // compositing wrapper (consumed by apply_op), and one value must
    // not silently drive both.
    d.drop_shadow(
        l,
        f,
        gi(op, "dx", 1),
        gi(op, "dy", 1),
        col(op, "color"),
        op.get("shadow_opacity")
            .and_then(|v| v.as_u64())
            .unwrap_or(160) as u8,
        gi(op, "blur", 0),
    )
}

fn op_bevel(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.bevel(l, f, col(op, "light"), col(op, "dark"), gi(op, "depth", 1))
}

fn op_shade(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.shade(
        l,
        f,
        op.get("light_dir")
            .and_then(|v| v.as_str())
            .unwrap_or("top-left"),
        gi(op, "steps", 1),
        region_val(op.get("region")),
        op.get("mode").and_then(|v| v.as_str()).unwrap_or("both"),
        op.get("ramp").map(|v| colors_val(Some(v))),
    )
}

fn op_form(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.form(
        l,
        f,
        op.get("light_dir")
            .and_then(|v| v.as_str())
            .unwrap_or("top-left"),
        op.get("form").and_then(|v| v.as_str()).unwrap_or("sphere"),
        region_val(op.get("region")),
        op.get("ramp").map(|v| colors_val(Some(v))),
        op.get("strength").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
    )
}

fn op_dither(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.dither(
        l,
        f,
        // Default to the whole canvas when no explicit region is provided.
        region_val(op.get("region")).unwrap_or((
            0,
            0,
            d.meta().w as i32 - 1,
            d.meta().h as i32 - 1,
        )),
        col(op, "color_a"),
        col(op, "color_b"),
        op.get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("bayer4"),
        op.get("density").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
        gb(op, "only_existing", false),
    )
}

fn op_pixel_perfect(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.pixel_perfect(
        l,
        f,
        region_val(op.get("region")),
        op.get("color").map(|_| col(op, "color")),
    )
    .map(|_| ())
}

fn op_flip(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.flip(l, f, gb(op, "horizontal", true))
}

fn op_shift(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.shift(
        l,
        f,
        gi(op, "dx", 0),
        gi(op, "dy", 0),
        gb(op, "wrap", false),
    )
}

fn op_symmetry(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.symmetry(
        l,
        f,
        op.get("vertical")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        op.get("horizontal")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        gb(op, "keep_left", true),
        gb(op, "keep_top", true),
    )
}

fn op_quantize(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.quantize(
        l,
        f,
        colors_val(op.get("colors")),
        op.get("max_colors").and_then(|v| v.as_u64()).unwrap_or(16) as usize,
    )
    .map(|_| ())
}

fn op_replace_color(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.replace_color(l, f, col(op, "from"), col(op, "to"), gi(op, "tolerance", 0))
}

fn op_adjust(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.adjust(
        l,
        f,
        region_val(op.get("region")),
        op.get("hue").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        op.get("sat").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        op.get("lum").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
    )
}

fn op_gradient_map(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.gradient_map(
        l,
        f,
        stops_val(op.get("stops")),
        region_val(op.get("region")),
    )
}

// -- operation JSON parsing helpers -----------------------------------------

fn rgba_val(v: Option<&Value>) -> [u8; 4] {
    if let Some(a) = v.and_then(|x| x.as_array()) {
        let g = |i: usize, d: u8| {
            a.get(i)
                .and_then(|n| n.as_u64())
                .map(|n| n as u8)
                .unwrap_or(d)
        };
        return [
            g(0, 0),
            g(1, 0),
            g(2, 0),
            a.get(3)
                .and_then(|n| n.as_u64())
                .map(|n| n as u8)
                .unwrap_or(255),
        ];
    }
    [0, 0, 0, 255]
}

/// Parse `[[x,y], ...]` or `[[x,y,width], ...]` into stroke vertices, filling
/// the missing width with `default_w`. Points shorter than 2 are dropped.
fn points3f_val(v: Option<&Value>, default_w: f32) -> Vec<(f32, f32, f32)> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_array())
                .filter(|pt| pt.len() >= 2)
                .map(|pt| {
                    // Parse as f64 so fractional stroke points survive to the
                    // sub-pixel coverage core instead of truncating to integers.
                    let g = |i: usize, d: f32| {
                        pt.get(i)
                            .and_then(|n| n.as_f64())
                            .map(|n| n as f32)
                            .unwrap_or(d)
                    };
                    (g(0, 0.0), g(1, 0.0), g(2, default_w))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn points_val(v: Option<&Value>) -> Vec<(i32, i32)> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_array())
                .map(|pt| {
                    let g = |i: usize| pt.get(i).and_then(|n| n.as_i64()).unwrap_or(0) as i32;
                    (g(0), g(1))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `[[x,y], ...]` into float vertices — curve control points keep their
/// sub-pixel precision (unlike `points_val`, which truncates for pixel ops).
fn points2f_val(v: Option<&Value>) -> Vec<(f32, f32)> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_array())
                .filter(|pt| pt.len() >= 2)
                .map(|pt| {
                    let g = |i: usize| pt.get(i).and_then(|n| n.as_f64()).unwrap_or(0.0) as f32;
                    (g(0), g(1))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse gradient stops `[{"pos":0.0,"color":[r,g,b,a]}, ...]`.
fn stops_val(v: Option<&Value>) -> Vec<(f32, [u8; 4])> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|s| {
                    (
                        s.get("pos").and_then(|n| n.as_f64()).unwrap_or(0.0) as f32,
                        rgba_val(s.get("color")),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a colour list `[[r,g,b,a], ...]` (for scatter).
fn colors_val(v: Option<&Value>) -> Vec<[u8; 4]> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().map(|c| rgba_val(Some(c))).collect())
        .unwrap_or_default()
}

/// Parse an optional `[x0,y0,x1,y1]` clip region.
fn region_val(v: Option<&Value>) -> Option<(i32, i32, i32, i32)> {
    let a = v.and_then(|x| x.as_array()).filter(|a| a.len() >= 4)?;
    let g = |i: usize| a[i].as_i64().unwrap_or(0) as i32;
    Some((g(0), g(1), g(2), g(3)))
}

/// The recognized keys for an operation kind: `(required, optional)`, read off
/// the [`OPS`] table. Returns None for an unknown op kind.
pub(super) fn op_keys(kind: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    find_op(kind).map(|s| (s.required, s.optional))
}

/// Strict colour-array parse: `v` must be `[r,g,b]` or `[r,g,b,a]` with every
/// component an integer 0..=255; the alpha defaults to 255. The ONE shape check
/// for caller-supplied colours — the operation validator and Studio's paint-grid
/// legend parser share it (two hand-synced copies drift).
pub fn color_array(v: &Value) -> Option<[u8; 4]> {
    let a = v.as_array()?;
    if !(3..=4).contains(&a.len()) {
        return None;
    }
    let mut out = [255u8; 4];
    for (i, c) in a.iter().enumerate() {
        out[i] = u8::try_from(c.as_u64()?).ok()?;
    }
    Some(out)
}

fn validate_i32(kind: &str, key: &str, value: &Value) -> Result<(), String> {
    let Some(number) = value.as_i64() else {
        return Err(format!(
            "operation ({kind}): '{key}' must be a 32-bit integer, got {value}"
        ));
    };
    i32::try_from(number).map(|_| ()).map_err(|_| {
        format!("operation ({kind}): '{key}' is outside the 32-bit integer range: {number}")
    })
}

fn validate_f32(kind: &str, key: &str, value: &Value) -> Result<(), String> {
    let Some(number) = value.as_f64() else {
        return Err(format!(
            "operation ({kind}): '{key}' must be a number, got {value}"
        ));
    };
    if !number.is_finite() || number.abs() > f32::MAX as f64 {
        return Err(format!(
            "operation ({kind}): '{key}' is outside the finite 32-bit float range: {number}"
        ));
    }
    Ok(())
}

fn validate_color_list(kind: &str, key: &str, value: &Value) -> Result<(), String> {
    let colors = value.as_array().ok_or_else(|| {
        format!("operation ({kind}): '{key}' must be an array of colour arrays, got {value}")
    })?;
    for (index, color) in colors.iter().enumerate() {
        if color_array(color).is_none() {
            return Err(format!(
                "operation ({kind}): '{key}[{index}]' must be [r,g,b] or [r,g,b,a] with 0..=255 values, got {color}"
            ));
        }
    }
    Ok(())
}

fn validate_region_value(kind: &str, value: &Value) -> Result<(), String> {
    let region = value
        .as_array()
        .filter(|items| items.len() == 4)
        .ok_or_else(|| {
            format!("operation ({kind}): 'region' must be exactly [x0,y0,x1,y1], got {value}")
        })?;
    for (index, coordinate) in region.iter().enumerate() {
        validate_i32(kind, &format!("region[{index}]"), coordinate)?;
    }
    Ok(())
}

fn validate_points(kind: &str, value: &Value) -> Result<(), String> {
    let points = value
        .as_array()
        .ok_or_else(|| format!("operation ({kind}): 'points' must be an array, got {value}"))?;
    let floating = matches!(kind, "stroke" | "curve");
    for (point_index, point) in points.iter().enumerate() {
        let point = point.as_array().ok_or_else(|| {
            format!("operation ({kind}): 'points[{point_index}]' must be an array, got {point}")
        })?;
        let valid_len = if kind == "stroke" {
            (2..=3).contains(&point.len())
        } else {
            point.len() == 2
        };
        if !valid_len {
            return Err(format!(
                "operation ({kind}): 'points[{point_index}]' must contain {} values, got {}",
                if kind == "stroke" { "2 or 3" } else { "2" },
                point.len()
            ));
        }
        for (coordinate_index, coordinate) in point.iter().enumerate() {
            let key = format!("points[{point_index}][{coordinate_index}]");
            if floating {
                validate_f32(kind, &key, coordinate)?;
            } else {
                validate_i32(kind, &key, coordinate)?;
            }
        }
    }
    Ok(())
}

fn validate_stops(kind: &str, value: &Value) -> Result<(), String> {
    let stops = value
        .as_array()
        .ok_or_else(|| format!("operation ({kind}): 'stops' must be an array, got {value}"))?;
    if !(1..=MAX_GRADIENT_STOPS).contains(&stops.len()) {
        return Err(format!(
            "operation ({kind}): 'stops' must contain 1..={MAX_GRADIENT_STOPS} entries, got {}",
            stops.len()
        ));
    }
    for (index, stop) in stops.iter().enumerate() {
        let object = stop.as_object().ok_or_else(|| {
            format!("operation ({kind}): 'stops[{index}]' must be an object, got {stop}")
        })?;
        let unexpected: Vec<&str> = object
            .keys()
            .map(String::as_str)
            .filter(|key| !matches!(*key, "pos" | "color"))
            .collect();
        if !unexpected.is_empty() {
            return Err(format!(
                "operation ({kind}): 'stops[{index}]' has unknown keys {}",
                unexpected.join(",")
            ));
        }
        let position = object
            .get("pos")
            .ok_or_else(|| format!("operation ({kind}): 'stops[{index}]' is missing 'pos'"))?;
        validate_f32(kind, &format!("stops[{index}].pos"), position)?;
        let color = object
            .get("color")
            .ok_or_else(|| format!("operation ({kind}): 'stops[{index}]' is missing 'color'"))?;
        if color_array(color).is_none() {
            return Err(format!(
                "operation ({kind}): 'stops[{index}].color' must be [r,g,b] or [r,g,b,a] with 0..=255 values, got {color}"
            ));
        }
    }
    Ok(())
}

fn validate_tip(kind: &str, value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("operation ({kind}): 'tip' must be an object, got {value}"))?;
    let unexpected: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !matches!(*key, "w" | "h" | "pixels"))
        .collect();
    if !unexpected.is_empty() {
        return Err(format!(
            "operation ({kind}): 'tip' has unknown keys {}",
            unexpected.join(",")
        ));
    }
    let dimension = |key: &str| -> Result<u32, String> {
        let value = object
            .get(key)
            .ok_or_else(|| format!("operation ({kind}): 'tip' is missing '{key}'"))?;
        let raw = value.as_u64().ok_or_else(|| {
            format!("operation ({kind}): 'tip.{key}' must be a positive integer, got {value}")
        })?;
        let out = u32::try_from(raw)
            .map_err(|_| format!("operation ({kind}): 'tip.{key}' exceeds u32: {raw}"))?;
        if out == 0 {
            return Err(format!(
                "operation ({kind}): 'tip.{key}' must be at least 1"
            ));
        }
        Ok(out)
    };
    let (width, height) = (dimension("w")?, dimension("h")?);
    raster::checked_rgba_dimensions("stamp tip", width as u64, height as u64)?;
    let pixels = object
        .get("pixels")
        .ok_or_else(|| format!("operation ({kind}): 'tip' is missing 'pixels'"))?;
    validate_color_list(kind, "tip.pixels", pixels)?;
    let actual = pixels.as_array().map_or(0, Vec::len);
    let expected = width as usize * height as usize;
    if actual != expected {
        return Err(format!(
            "operation ({kind}): 'tip.pixels' must have w*h={expected} colors, got {actual}"
        ));
    }
    Ok(())
}

/// Strictly validate one operation object before it runs: the `op` key must name
/// a known kind, every required key must be present, and no unrecognized keys
/// may appear (typos / wrong-shape params would otherwise be silently defaulted).
pub fn validate_op(op: &Value) -> Result<(), String> {
    let obj = op.as_object().ok_or("operation must be a JSON object")?;
    let kind = obj
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or("operation is missing the 'op' key")?;
    let (required, optional) =
        op_keys(kind).ok_or_else(|| format!("unknown operation '{kind}'"))?;
    // Keys every op accepts on top of its own params.
    let common = ["op", "opacity", "blend_mode", "erase"];
    let known = |k: &str| common.contains(&k) || required.contains(&k) || optional.contains(&k);
    let bad: Vec<&str> = obj
        .keys()
        .map(|k| k.as_str())
        .filter(|k| !known(k))
        .collect();
    if !bad.is_empty() {
        let mut allowed: Vec<&str> = required.iter().chain(optional.iter()).copied().collect();
        return Err(format!(
            "operation ({}) has unknown keys {} — {} takes {}",
            kind,
            bad.join(","),
            kind,
            if allowed.is_empty() {
                "(no params)".to_string()
            } else {
                allowed.sort_unstable();
                allowed.join(",")
            }
        ));
    }
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| !obj.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "operation ({}) is missing required keys {}",
            kind,
            missing.join(",")
        ));
    }
    // Colour-typed keys must be well-formed [r,g,b(,a)] arrays. Without this
    // check a colour given as "#5e2a6e" or {"r":..} silently fell back to
    // BLACK — a wrong-but-plausible result an agent then burns calls
    // repainting around. Malformed input errors loudly instead.
    const COLOR_KEYS: [&str; 8] = [
        "color", "light", "dark", "color_a", "color_b", "from", "to", "colorize",
    ];
    for k in COLOR_KEYS {
        // `from`/`to` are colours only where the op's key table says so.
        if !(required.contains(&k) || optional.contains(&k)) {
            continue;
        }
        if let Some(v) = obj.get(k)
            && color_array(v).is_none()
        {
            return Err(format!(
                "operation ({kind}): '{k}' must be a colour array [r,g,b] or [r,g,b,a] with 0..=255 values, got {v}"
            ));
        }
    }
    // 0..=255 scalars: the wrapper `opacity` and drop_shadow's
    // `shadow_opacity` both funnel into u8 — a value like 300
    // used to truncate (300 → 44) into a wrong-but-plausible result.
    for k in ["opacity", "shadow_opacity"] {
        if let Some(v) = obj.get(k)
            && v.as_u64().is_none_or(|n| n > 255)
        {
            return Err(format!(
                "operation ({kind}): '{k}' must be an integer 0..=255, got {v}"
            ));
        }
    }
    if let Some(value) = obj.get("blend_mode") {
        let name = value
            .as_str()
            .ok_or_else(|| format!("operation ({kind}): 'blend_mode' must be a string"))?;
        name.parse::<raster::Blend>()
            .map_err(|error| format!("operation ({kind}): {error}"))?;
    }
    if let Some(value) = obj.get("erase")
        && !value.is_boolean()
    {
        return Err(format!(
            "operation ({kind}): 'erase' must be a boolean, got {value}"
        ));
    }

    // Scalar values consumed through the compact parsing helpers must match
    // their actual executor type. Previously a string silently became the
    // helper's default and an i64 outside i32 wrapped through `as i32`.
    const I32_KEYS: [&str; 17] = [
        "x",
        "y",
        "x0",
        "y0",
        "x1",
        "y1",
        "cx",
        "cy",
        "rx",
        "ry",
        "size",
        "tolerance",
        "radius",
        "dx",
        "dy",
        "blur",
        "depth",
    ];
    for key in I32_KEYS {
        if let Some(value) = obj.get(key) {
            validate_i32(kind, key, value)?;
        }
    }
    if let Some(value) = obj.get("size")
        && value.as_i64().is_none_or(|size| size < 1)
    {
        return Err(format!(
            "operation ({kind}): 'size' must be an integer in 1..={}, got {value}",
            i32::MAX
        ));
    }
    if let Some(value) = obj.get("steps") {
        validate_i32(kind, "steps", value)?;
    }
    if kind == "symmetry" {
        for key in ["vertical", "horizontal"] {
            if let Some(value) = obj.get(key) {
                validate_i32(kind, key, value)?;
            }
        }
    }
    for key in ["w", "h"] {
        if let Some(value) = obj.get(key) {
            let raw = value.as_i64().ok_or_else(|| {
                format!(
                    "operation ({kind}): '{key}' must be a positive 32-bit integer, got {value}"
                )
            })?;
            if !(1..=i32::MAX as i64).contains(&raw) {
                return Err(format!(
                    "operation ({kind}): '{key}' must be in 1..={}, got {raw}",
                    i32::MAX
                ));
            }
        }
    }
    if let Some(value) = obj.get("turns")
        && value.as_u64().is_none_or(|number| number > u8::MAX as u64)
    {
        return Err(format!(
            "operation ({kind}): 'turns' must be an integer 0..={}, got {value}",
            u8::MAX
        ));
    }
    if let Some(value) = obj.get("octaves") {
        let valid = value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .is_some_and(|number| (1..=MAX_NOISE_OCTAVES).contains(&number));
        if !valid {
            return Err(format!(
                "operation ({kind}): 'octaves' must be an integer 1..={MAX_NOISE_OCTAVES}, got {value}"
            ));
        }
    }
    if let Some(value) = obj.get("max_colors") {
        let valid = value
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .is_some_and(|number| (1..=MAX_QUANTIZE_COLORS).contains(&number));
        if !valid {
            return Err(format!(
                "operation ({kind}): 'max_colors' must be an integer 1..={MAX_QUANTIZE_COLORS}, got {value}"
            ));
        }
    }
    if let Some(value) = obj.get("seed")
        && value.as_u64().is_none()
    {
        return Err(format!(
            "operation ({kind}): 'seed' must be a non-negative integer, got {value}"
        ));
    }

    for key in ["width", "density", "strength", "hue", "sat", "lum"] {
        if let Some(value) = obj.get(key) {
            validate_f32(kind, key, value)?;
        }
    }
    if kind == "noise"
        && let Some(value) = obj.get("scale")
    {
        validate_f32(kind, "scale", value)?;
    }

    const BOOL_KEYS: [&str; 10] = [
        "fill",
        "closed",
        "aa",
        "snap",
        "blend",
        "only_existing",
        "wrap",
        "keep_left",
        "keep_top",
        "horizontal",
    ];
    for key in BOOL_KEYS {
        if key == "horizontal" && kind == "symmetry" {
            continue;
        }
        if let Some(value) = obj.get(key)
            && !value.is_boolean()
        {
            return Err(format!(
                "operation ({kind}): '{key}' must be a boolean, got {value}"
            ));
        }
    }
    for key in [
        "kind",
        "dither",
        "text",
        "method",
        "light_dir",
        "mode",
        "form",
        "pattern",
    ] {
        if let Some(value) = obj.get(key)
            && !value.is_string()
        {
            return Err(format!(
                "operation ({kind}): '{key}' must be a string, got {value}"
            ));
        }
    }

    if let Some(value) = obj.get("points") {
        validate_points(kind, value)?;
    }
    if let Some(value) = obj.get("region") {
        validate_region_value(kind, value)?;
    }
    if let Some(value) = obj.get("stops") {
        validate_stops(kind, value)?;
    }
    for key in ["colors", "ramp"] {
        if let Some(value) = obj.get(key) {
            validate_color_list(kind, key, value)?;
            let count = value.as_array().map_or(0, Vec::len);
            if count > MAX_PALETTE_COLORS {
                return Err(format!(
                    "operation ({kind}): '{key}' may contain at most {MAX_PALETTE_COLORS} colours, got {count}"
                ));
            }
        }
    }
    if let Some(value) = obj.get("tip") {
        validate_tip(kind, value)?;
    }
    Ok(())
}
