//! Batched JSON drawing ops: dispatch, key registry and strict validation.

use image::Rgba;
use serde_json::Value;

use crate::raster;

use super::{AlphaSnap, Document};

impl Document {
    /// Apply one batched drawing op described by a JSON object `{"op": "...", ...}`.
    /// Lets a headless client send many ordered edits in a single tool call.
    ///
    /// Optional per-op `"opacity"` (0..255) and `"blend_mode"` (a layer blend
    /// name) composite the op's result instead of overwriting: the op is run,
    /// then the pixels it changed are re-composited over the pre-op cel with the
    /// given opacity/mode (snapshot-diff, so any op gains blend/opacity).
    /// `"erase": true` turns the op into an ERASER instead: every pixel the op
    /// touched becomes transparent — any shape (pencil/line/ellipse/fill/…) can
    /// punch a hole, which no colour trick could do (drawing [0,0,0,0] is a
    /// no-op under source-over).
    pub fn apply_op(&mut self, layer: usize, frame: usize, op: &Value) -> Result<(), String> {
        let opacity = op.get("opacity").and_then(|v| v.as_u64()).map(|v| v as u8);
        let mode = op
            .get("blend_mode")
            .and_then(|v| v.as_str())
            .map(raster::parse_blend);
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
            .ok_or("batch op missing 'op'")?;
        let spec = find_op(name).ok_or_else(|| format!("unknown batch op '{name}'"))?;
        (spec.run)(self, layer, frame, op)?;
        // Continuous-tone FX push a locked palette into hundreds of colours; re-snap
        // onto it by default (parity with gradient/glow) so the result stays crisp
        // pixel art. Opt out per op with `snap:false`. Hard-edged / already-on-palette
        // ops (outline, dither, pixel_perfect, quantize, adjust) are excluded.
        if matches!(name, "blur" | "drop_shadow" | "bevel" | "form" | "shade")
            && gb(op, "snap", true)
            && !self.meta.palette.is_empty()
        {
            let pal = self.meta.palette.clone();
            self.snap_to_palette(&pal, Some(layer), Some(frame), AlphaSnap::Preserve);
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

/// Which tool vocabulary an op belongs to. `doc_draw` adds new marks, `doc_fx`
/// reworks existing pixels; `BatchOnly` is callable only inside doc_batch
/// (glow — its on-palette snap is not a single-op form).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpSide {
    Draw,
    Fx,
    BatchOnly,
}

/// One batch op: its name, JSON schema (required/optional keys), side, and
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
    // -- batch-only --
    OpSpec {
        name: "glow",
        required: &[],
        optional: &["color", "radius", "intensity", "mode"],
        side: OpSide::BatchOnly,
        run: op_glow,
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
        _ => &FX,
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

/// The doc_fx vocabulary (`glow` is deliberately absent — batch-only).
pub fn fx_ops() -> &'static [&'static str] {
    side_ops(OpSide::Fx)
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
    // Parity with the standalone doc_gradient: re-snap on-palette by default
    // when a palette is locked (the batch path used to skip it).
    if gb(op, "snap", true) && !d.meta().palette.is_empty() {
        let pal = d.meta().palette.clone();
        d.snap_to_palette(&pal, Some(l), Some(f), AlphaSnap::Preserve);
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
    // `shadow_opacity`, not `opacity`: the plain key is the batch-wide
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
        // In a batch the studio applies any active selection as a mask,
        // so default to the whole canvas when no explicit region.
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

fn op_glow(d: &mut Document, l: usize, f: usize, op: &Value) -> Result<(), String> {
    d.glow(
        l,
        f,
        op.get("color").map(|_| col(op, "color")),
        gi(op, "radius", 2),
        op.get("intensity").and_then(|v| v.as_u64()).unwrap_or(180) as u8,
        op.get("mode").and_then(|v| v.as_str()).unwrap_or("screen"),
    )
}

// -- batch-op JSON parsing helpers ------------------------------------------

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

/// The recognized keys for a batch op kind: `(required, optional)`, read off
/// the [`OPS`] table. Returns None for an unknown op kind.
pub(super) fn batch_op_keys(
    kind: &str,
) -> Option<(&'static [&'static str], &'static [&'static str])> {
    find_op(kind).map(|s| (s.required, s.optional))
}

/// Strict colour-array parse: `v` must be `[r,g,b]` or `[r,g,b,a]` with every
/// component an integer 0..=255; the alpha defaults to 255. The ONE shape check
/// for caller-supplied colours — the batch validator and the studio's paint-grid
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

/// Strictly validate one batch op object before it runs: the `op` key must name
/// a known kind, every required key must be present, and no unrecognized keys
/// may appear (typos / wrong-shape params would otherwise be silently defaulted).
/// `idx` is the op's position in the batch, used only for the error message.
pub fn validate_batch_op(idx: usize, op: &Value) -> Result<(), String> {
    let obj = op
        .as_object()
        .ok_or_else(|| format!("op[{}]: each op must be a JSON object", idx))?;
    let kind = obj
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("op[{}]: missing 'op' key naming the op kind", idx))?;
    let (required, optional) =
        batch_op_keys(kind).ok_or_else(|| format!("op[{}]: unknown op '{}'", idx, kind))?;
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
            "op[{}] ({}): unknown keys {} — {} takes {}",
            idx,
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
            "op[{}] ({}): missing required keys {}",
            idx,
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
        if let Some(v) = obj.get(k) {
            if color_array(v).is_none() {
                return Err(format!(
                    "op[{}] ({}): '{}' must be a colour array [r,g,b] or [r,g,b,a] with 0..=255 values, got {}",
                    idx, kind, k, v
                ));
            }
        }
    }
    // 0..=255 scalars: the wrapper `opacity`, glow's `intensity` and
    // drop_shadow's `shadow_opacity` all funnel into u8 — a value like 300
    // used to truncate (300 → 44) into a wrong-but-plausible result.
    for k in ["opacity", "intensity", "shadow_opacity"] {
        if let Some(v) = obj.get(k) {
            if v.as_u64().is_none_or(|n| n > 255) {
                return Err(format!(
                    "op[{idx}] ({kind}): '{k}' must be an integer 0..=255, got {v}"
                ));
            }
        }
    }
    Ok(())
}
