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
    pub fn apply_op(&mut self, layer: usize, frame: usize, op: &Value) -> Result<(), String> {
        let opacity = op.get("opacity").and_then(|v| v.as_u64()).map(|v| v as u8);
        let mode = op
            .get("blend_mode")
            .and_then(|v| v.as_str())
            .map(raster::parse_blend);
        if opacity.is_none() && mode.is_none() {
            return self.apply_op_raw(layer, frame, op);
        }
        let before = self.cel_canvas(layer, frame)?.clone();
        self.apply_op_raw(layer, frame, op)?;
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

    /// The op-name dispatch table. MUST stay in lockstep with `batch_op_keys`:
    /// every op name handled here must have an entry there (and vice versa) — the
    /// two are hand-synced, so adding/renaming an op means editing both.
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
        let gi = |k: &str, d: i64| op.get(k).and_then(|v| v.as_i64()).unwrap_or(d) as i32;
        let gb = |k: &str, d: bool| op.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
        let col = |k: &str| rgba_val(op.get(k));
        let outcome = match name {
            "pencil" => self.pencil(
                layer,
                frame,
                &points_val(op.get("points")),
                col("color"),
                gi("size", 1),
            ),
            "line" => self.line(
                layer,
                frame,
                gi("x0", 0),
                gi("y0", 0),
                gi("x1", 0),
                gi("y1", 0),
                col("color"),
                gi("size", 1),
            ),
            "rect" => self.rect(
                layer,
                frame,
                gi("x0", 0),
                gi("y0", 0),
                gi("x1", 0),
                gi("y1", 0),
                col("color"),
                gb("fill", false),
                gi("size", 1),
            ),
            "ellipse" => self.ellipse(
                layer,
                frame,
                gi("cx", 0),
                gi("cy", 0),
                gi("rx", 1),
                gi("ry", 1),
                col("color"),
                gb("fill", false),
            ),
            "polyline" => self.polyline(
                layer,
                frame,
                &points_val(op.get("points")),
                col("color"),
                gi("size", 1),
                gb("closed", false),
            ),
            "polygon" => self.polygon(
                layer,
                frame,
                &points_val(op.get("points")),
                col("color"),
                gb("fill", true),
            ),
            "stroke" => {
                let w = op.get("width").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32;
                self.stroke_f(
                    layer,
                    frame,
                    &points3f_val(op.get("points"), w),
                    col("color"),
                    gb("aa", true),
                    gb("snap", true),
                )
            }
            "fill" | "bucket" => self.bucket_fill(
                layer,
                frame,
                gi("x", 0),
                gi("y", 0),
                col("color"),
                gi("tolerance", 0),
            ),
            "replace_color" => {
                self.replace_color(layer, frame, col("from"), col("to"), gi("tolerance", 0))
            }
            "flip" => self.flip(layer, frame, gb("horizontal", true)),
            "shift" => self.shift(layer, frame, gi("dx", 0), gi("dy", 0), gb("wrap", false)),
            "blur" => self.blur(layer, frame, gi("radius", 1), region_val(op.get("region"))),
            "quantize" => self
                .quantize(
                    layer,
                    frame,
                    colors_val(op.get("colors")),
                    op.get("max_colors").and_then(|v| v.as_u64()).unwrap_or(16) as usize,
                )
                .map(|_| ()),
            "outline" => self.outline_cel(layer, frame, col("color"), gb("aa", false)),
            // `shadow_opacity`, not `opacity`: the plain key is the batch-wide
            // compositing wrapper (consumed by apply_op), and one value must
            // not silently drive both.
            "drop_shadow" => self.drop_shadow(
                layer,
                frame,
                gi("dx", 1),
                gi("dy", 1),
                col("color"),
                op.get("shadow_opacity")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(160) as u8,
                gi("blur", 0),
            ),
            "glow" => self.glow(
                layer,
                frame,
                op.get("color").map(|_| col("color")),
                gi("radius", 2),
                op.get("intensity").and_then(|v| v.as_u64()).unwrap_or(180) as u8,
                op.get("mode").and_then(|v| v.as_str()).unwrap_or("screen"),
            ),
            "bevel" => self.bevel(layer, frame, col("light"), col("dark"), gi("depth", 1)),
            "fill_cel" => self.fill_cel(layer, frame, col("color")),
            "clear_cel" => self.clear_cel(layer, frame),
            "gradient" => {
                self.gradient(
                    layer,
                    frame,
                    op.get("kind").and_then(|v| v.as_str()).unwrap_or("linear"),
                    gi("x0", 0),
                    gi("y0", 0),
                    gi("x1", 0),
                    gi("y1", 0),
                    stops_val(op.get("stops")),
                    op.get("dither").and_then(|v| v.as_str()).unwrap_or("none"),
                    op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
                    region_val(op.get("region")),
                    gb("blend", true),
                )?;
                // Parity with the standalone doc_gradient: re-snap on-palette by
                // default when a palette is locked (the batch path used to skip it).
                if gb("snap", true) && !self.meta.palette.is_empty() {
                    let pal = self.meta.palette.clone();
                    self.snap_to_palette(&pal, Some(layer), Some(frame), AlphaSnap::Preserve);
                }
                Ok(())
            }
            "scatter" => self.scatter(
                layer,
                frame,
                gi("x0", 0),
                gi("y0", 0),
                gi("x1", 0),
                gi("y1", 0),
                &colors_val(op.get("colors")),
                op.get("density").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32,
                op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
                gi("size", 1),
            ),
            "symmetry" => self.symmetry(
                layer,
                frame,
                op.get("vertical")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                op.get("horizontal")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                gb("keep_left", true),
                gb("keep_top", true),
            ),
            "adjust" => self.adjust(
                layer,
                frame,
                region_val(op.get("region")),
                op.get("hue").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                op.get("sat").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                op.get("lum").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            ),
            "noise" => self.noise(
                layer,
                frame,
                op.get("kind").and_then(|v| v.as_str()).unwrap_or("cloud"),
                gi("x0", 0),
                gi("y0", 0),
                gi("x1", 0),
                gi("y1", 0),
                op.get("scale").and_then(|v| v.as_f64()).unwrap_or(8.0) as f32,
                op.get("octaves").and_then(|v| v.as_u64()).unwrap_or(4) as u32,
                op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
                stops_val(op.get("stops")),
                gb("blend", false),
            ),
            "shade" => self.shade(
                layer,
                frame,
                op.get("light_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("top-left"),
                gi("steps", 1),
                region_val(op.get("region")),
                op.get("mode").and_then(|v| v.as_str()).unwrap_or("both"),
                op.get("ramp").map(|v| colors_val(Some(v))),
            ),
            "form" => self.form(
                layer,
                frame,
                op.get("light_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("top-left"),
                op.get("form").and_then(|v| v.as_str()).unwrap_or("sphere"),
                region_val(op.get("region")),
                op.get("ramp").map(|v| colors_val(Some(v))),
                op.get("strength").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            ),
            "dither" => self.dither(
                layer,
                frame,
                // In a batch the studio applies any active selection as a mask,
                // so default to the whole canvas when no explicit region.
                region_val(op.get("region")).unwrap_or((
                    0,
                    0,
                    self.meta.w as i32 - 1,
                    self.meta.h as i32 - 1,
                )),
                col("color_a"),
                col("color_b"),
                op.get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("bayer4"),
                op.get("density").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
                gb("only_existing", false),
            ),
            "gradient_map" => self.gradient_map(
                layer,
                frame,
                stops_val(op.get("stops")),
                region_val(op.get("region")),
            ),
            "pixel_perfect" => self
                .pixel_perfect(
                    layer,
                    frame,
                    region_val(op.get("region")),
                    op.get("color").map(|_| col("color")),
                )
                .map(|_| ()),
            "text" => self
                .text(
                    layer,
                    frame,
                    gi("x", 0),
                    gi("y", 0),
                    op.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                    col("color"),
                    gi("size", 1),
                )
                .map(|_| ()),
            other => Err(format!("unknown batch op '{}'", other)),
        };
        outcome?;
        // Continuous-tone FX push a locked palette into hundreds of colours; re-snap
        // onto it by default (parity with gradient/glow) so the result stays crisp
        // pixel art. Opt out per op with `snap:false`. Hard-edged / already-on-palette
        // ops (outline, dither, pixel_perfect, quantize, adjust) are excluded.
        if matches!(name, "blur" | "drop_shadow" | "bevel" | "form" | "shade")
            && gb("snap", true)
            && !self.meta.palette.is_empty()
        {
            let pal = self.meta.palette.clone();
            self.snap_to_palette(&pal, Some(layer), Some(frame), AlphaSnap::Preserve);
        }
        Ok(())
    }
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

/// The recognized keys for a batch op kind: `(required, optional)`. Every op
/// additionally accepts the compositing wrapper keys `opacity`/`blend_mode`
/// (and the discriminator `op`). Returns None for an unknown op kind.
///
/// MUST stay in lockstep with `apply_op_raw`'s match: every op name listed here
/// must be dispatched there (and vice versa) — the two tables are hand-synced.
pub(super) fn batch_op_keys(
    kind: &str,
) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match kind {
        "pencil" => (&["points", "color"], &["size"]),
        "line" => (&["x0", "y0", "x1", "y1", "color"], &["size"]),
        "rect" => (&["x0", "y0", "x1", "y1", "color"], &["fill", "size"]),
        "ellipse" => (&["cx", "cy", "rx", "ry", "color"], &["fill"]),
        "polyline" => (&["points", "color"], &["size", "closed"]),
        "polygon" => (&["points", "color"], &["fill"]),
        "stroke" => (&["points", "color"], &["width", "aa", "snap"]),
        "fill" | "bucket" => (&["x", "y", "color"], &["tolerance"]),
        "replace_color" => (&["from", "to"], &["tolerance"]),
        "flip" => (&[], &["horizontal"]),
        "shift" => (&[], &["dx", "dy", "wrap"]),
        "blur" => (&["radius"], &["region", "snap"]),
        "quantize" => (&["colors"], &["max_colors"]),
        "outline" => (&["color"], &["aa"]),
        "drop_shadow" => (&["color"], &["dx", "dy", "blur", "snap", "shadow_opacity"]),
        "glow" => (&[], &["color", "radius", "intensity", "mode"]),
        "bevel" => (&["light", "dark"], &["depth", "snap"]),
        "fill_cel" => (&["color"], &[]),
        "clear_cel" => (&[], &[]),
        "gradient" => (
            &["stops"],
            &[
                "kind", "x0", "y0", "x1", "y1", "dither", "seed", "region", "blend", "snap",
            ],
        ),
        "scatter" => (
            &["colors", "x0", "y0", "x1", "y1"],
            &["density", "seed", "size"],
        ),
        "symmetry" => (&[], &["vertical", "horizontal", "keep_left", "keep_top"]),
        "adjust" => (&[], &["region", "hue", "sat", "lum"]),
        "gradient_map" => (&["stops"], &["region"]),
        "noise" => (
            &["stops", "x0", "y0", "x1", "y1"],
            &["kind", "scale", "octaves", "seed", "blend"],
        ),
        "shade" => (
            &[],
            &["light_dir", "steps", "region", "mode", "ramp", "snap"],
        ),
        "form" => (
            &[],
            &["form", "light_dir", "region", "ramp", "strength", "snap"],
        ),
        "dither" => (
            &["color_a", "color_b"],
            &["region", "pattern", "density", "only_existing"],
        ),
        "pixel_perfect" => (&[], &["region", "color"]),
        "text" => (&["x", "y", "text", "color"], &["size"]),
        _ => return None,
    })
}

/// The draw/fx partition of the batch ops, next to the registry so a new op
/// lands in exactly one list in the same file. `doc_draw` adds new marks,
/// `doc_fx` reworks existing pixels; `fill_cel`/`clear_cel` are draw-side,
/// `glow` is deliberately absent (its on-palette snap is not a batch op).
pub const DRAW_OPS: &[&str] = &[
    "pencil",
    "line",
    "rect",
    "ellipse",
    "polyline",
    "polygon",
    "stroke",
    "fill",
    "bucket",
    "gradient",
    "scatter",
    "noise",
    "text",
    "fill_cel",
    "clear_cel",
];
pub const FX_OPS: &[&str] = &[
    "blur",
    "outline",
    "drop_shadow",
    "bevel",
    "shade",
    "form",
    "dither",
    "pixel_perfect",
    "flip",
    "shift",
    "symmetry",
    "quantize",
    "replace_color",
    "adjust",
    "gradient_map",
];

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
    let common = ["op", "opacity", "blend_mode"];
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
    const COLOR_KEYS: [&str; 7] = ["color", "light", "dark", "color_a", "color_b", "from", "to"];
    let is_color = |v: &Value| {
        v.as_array().is_some_and(|a| {
            (3..=4).contains(&a.len()) && a.iter().all(|n| n.as_u64().is_some_and(|n| n <= 255))
        })
    };
    for k in COLOR_KEYS {
        // `from`/`to` are colours only where the op's key table says so.
        if !(required.contains(&k) || optional.contains(&k)) {
            continue;
        }
        if let Some(v) = obj.get(k) {
            if !is_color(v) {
                return Err(format!(
                    "op[{}] ({}): '{}' must be a colour array [r,g,b] or [r,g,b,a] with 0..=255 values, got {}",
                    idx, kind, k, v
                ));
            }
        }
    }
    Ok(())
}
