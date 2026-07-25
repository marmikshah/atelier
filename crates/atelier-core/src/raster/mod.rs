//! Pure pixel / colour / noise functions with no `Document` knowledge.
//!
//! Free functions over `RgbaImage` and `[u8; 4]` colours: drawing primitives,
//! blend/composite, colour-space conversion, procedural noise, palette
//! quantisation and shading ramps. Everything here is stateless and reusable.

use image::{Rgba, RgbaImage};

mod colour;
mod noise;
mod transform;

// Explicit surface instead of globs: exactly what the rest of the workspace
// uses, so the module's API is one readable list.
pub use colour::{
    PaletteLab, close, close_rgb, hsl_to_rgb, hue_deg, luma, make_ramp, make_ramp_oklch,
    median_cut, median_cut_weighted, nearest_oklab, oklab_delta, oklab_to_oklch, oklab_to_srgb,
    oklch_to_oklab, rgb_to_hsl, saturation, shade_hsl, shade_ramp, srgb_to_oklab,
};
pub use noise::{
    dither_threshold, fbm, hash2, perlin, ramp_dither_threshold, sample_gradient, voronoi,
};
pub use transform::{ScaleMethod, interior_distance, remove_background, rotate_quarters, scale};

// -- drawing helpers (overwrite semantics; alpha 0 = erase) -----------------

/// Overwrite a single pixel if (x,y) is inside the image bounds.
pub fn put(img: &mut RgbaImage, x: i32, y: i32, color: [u8; 4]) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, Rgba(color));
    }
}

/// Stamp a `size`×`size` square brush centred at (cx, cy).
pub fn brush(img: &mut RgbaImage, cx: i32, cy: i32, color: [u8; 4], size: i32) {
    // A brush bigger than the canvas covers it — the size is raw caller input,
    // and a size×size inner loop over billions of cells hung the server.
    let size = size.min(img.width().max(img.height()) as i32);
    let o = size / 2;
    for dy in 0..size {
        for dx in 0..size {
            put(img, cx - o + dx, cy - o + dy, color);
        }
    }
}

/// Clip a segment to a rectangle (Liang–Barsky), returning the visible portion.
/// Endpoints are raw caller input; this is what lets `draw_line` bound its walk
/// to the canvas instead of stepping across an arbitrary i32 span. The math is
/// f64: at 1e9 the f32 grid is 64px wide, which rounded the clipped endpoints
/// to the same pixel and erased the whole segment.
fn clip_segment(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    lo: f64,
    hi_x: f64,
    hi_y: f64,
) -> Option<(i32, i32, i32, i32)> {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    let mut clip = |p: f64, q: f64| -> bool {
        if p == 0.0 {
            return q >= 0.0; // parallel: inside iff on the visible side
        }
        let r = q / p;
        if p < 0.0 {
            if r > t1 {
                return false;
            }
            if r > t0 {
                t0 = r;
            }
        } else {
            if r < t0 {
                return false;
            }
            if r < t1 {
                t1 = r;
            }
        }
        true
    };
    // lo applies to both axes' lower edge (upper edges differ: w-1, h-1).
    if !clip(-dx, x0 - lo) || !clip(dx, hi_x - x0) || !clip(-dy, y0 - lo) || !clip(dy, hi_y - y0) {
        return None;
    }
    Some((
        (x0 + t0 * dx).round() as i32,
        (y0 + t0 * dy).round() as i32,
        (x0 + t1 * dx).round() as i32,
        (y0 + t1 * dy).round() as i32,
    ))
}

/// Bresenham line from (x0,y0) to (x1,y1) stamped with a `size` brush.
pub fn draw_line(
    img: &mut RgbaImage,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: [u8; 4],
    size: i32,
) {
    // Clip to the canvas (padded by the brush radius) before walking: the raw
    // endpoints could span ~2^32 steps — and `(x1 - x0).abs()` overflowed i32 —
    // so one bad call wedged the server drawing nothing visible. In-canvas
    // output matches the unclipped walk to within a pixel at the clip boundary
    // (a re-anchored Bresenham can deviate by one step on rare near-diagonals)
    // — cosmetic, and the unclipped walk hung on exactly these inputs.
    let pad = (size.max(1) / 2 + 1) as f64;
    let lo = -pad;
    let (hi_x, hi_y) = (
        img.width() as f64 - 1.0 + pad,
        img.height() as f64 - 1.0 + pad,
    );
    let Some((x0, y0, x1, y1)) =
        clip_segment(x0 as f64, y0 as f64, x1 as f64, y1 as f64, lo, hi_x, hi_y)
    else {
        return; // entirely off-canvas
    };
    let (mut x0, mut y0) = (x0, y0);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        brush(img, x0, y0, color, size);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

/// Distance from point (px,py) to the segment a→b, plus the projection
/// parameter `t` clamped to [0,1] (0 at a, 1 at b). The geometric primitive the
/// variable-width stroke core is built on.
fn point_seg(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> (f32, f32) {
    let (vx, vy) = (bx - ax, by - ay);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= 1e-6 {
        0.0
    } else {
        (((px - ax) * vx + (py - ay) * vy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (ax + t * vx, ay + t * vy);
    let (dx, dy) = (px - cx, py - cy);
    ((dx * dx + dy * dy).sqrt(), t)
}

/// Rasterize a variable-width, anti-aliased stroke as the UNION of round-capped
/// capsules through `pts` (each `x, y, half_width` in pixels). This is the
/// "clean by construction" stroke core: the union makes the run connected with
/// no gaps, the per-vertex half-width gives taper, and the analytic coverage
/// (`clamp(half_width(t) + 0.5 − distance, 0, 1)`) gives a smooth edge instead of
/// a Bresenham staircase. Each covered pixel blends `color` over the existing
/// pixel weighted by coverage×alpha (so AA resolves against the real backdrop,
/// staying contiguous along the contour rather than scattering as speckle).
/// `aa=false` hard-thresholds coverage at 0.5 for a crisp — but still
/// union-connected and tapered — edge.
pub fn stroke_ribbon(img: &mut RgbaImage, pts: &[(f32, f32, f32)], color: [u8; 4], aa: bool) {
    if pts.is_empty() {
        return;
    }
    let (w, h) = (img.width() as i32, img.height() as i32);
    let max_hw = pts.iter().fold(0.5f32, |m, p| m.max(p.2));
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y, _) in pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let pad = max_hw + 1.0;
    let bx0 = ((x0 + 0.5 - pad).floor() as i32).max(0);
    let by0 = ((y0 + 0.5 - pad).floor() as i32).max(0);
    let bx1 = ((x1 + 0.5 + pad).ceil() as i32).min(w - 1);
    let by1 = ((y1 + 0.5 + pad).ceil() as i32).min(h - 1);
    // A stroke entirely off-canvas inverts the clamped bbox (bx1 < bx0), which
    // wrapped the size cast to ~4 billion and panicked `clamp(min > max)` in
    // splat — in release too. Nothing is on screen, so there is nothing to draw:
    // clip silently, like every other primitive.
    if bx1 < bx0 || by1 < by0 {
        return;
    }
    // Union (max) coverage accumulated per segment over its OWN padded bbox —
    // the whole-polyline bbox × every-segment scan made a long diagonal stroke
    // cost bbox-area × segments. The buffer is composited once at the end.
    let (bw_, bh_) = ((bx1 - bx0 + 1) as usize, (by1 - by0 + 1) as usize);
    let mut cov = vec![0.0f32; bw_ * bh_];
    let at = |px: i32, py: i32| ((py - by0) as usize) * bw_ + ((px - bx0) as usize);
    let splat = |ax: f32, ay: f32, aw: f32, bx: f32, by: f32, bw: f32, cov: &mut Vec<f32>| {
        let pad = aw.max(bw) + 1.0;
        let sx0 = (((ax.min(bx)) + 0.5 - pad).floor() as i32).clamp(bx0, bx1);
        let sy0 = (((ay.min(by)) + 0.5 - pad).floor() as i32).clamp(by0, by1);
        let sx1 = (((ax.max(bx)) + 0.5 + pad).ceil() as i32).clamp(bx0, bx1);
        let sy1 = (((ay.max(by)) + 0.5 + pad).ceil() as i32).clamp(by0, by1);
        for py in sy0..=sy1 {
            for px in sx0..=sx1 {
                let (sx, sy) = (px as f32 + 0.5, py as f32 + 0.5);
                let (d, t) = point_seg(sx, sy, ax + 0.5, ay + 0.5, bx + 0.5, by + 0.5);
                let hw = aw + (bw - aw) * t;
                let c = (hw + 0.5 - d).clamp(0.0, 1.0);
                let slot = &mut cov[at(px, py)];
                if c > *slot {
                    *slot = c;
                }
            }
        }
    };
    if pts.len() == 1 {
        let (x, y, hw) = pts[0];
        splat(x, y, hw, x, y, hw, &mut cov);
    } else {
        for win in pts.windows(2) {
            let (ax, ay, aw) = win[0];
            let (bx, by, bw) = win[1];
            splat(ax, ay, aw, bx, by, bw, &mut cov);
        }
    }
    for py in by0..=by1 {
        for px in bx0..=bx1 {
            let cov = cov[at(px, py)];
            let eff = if aa {
                cov
            } else if cov >= 0.5 {
                1.0
            } else {
                0.0
            };
            if eff <= 0.0 {
                continue;
            }
            let a = (color[3] as f32 / 255.0) * eff;
            if a <= 0.0 {
                continue;
            }
            let dst = img.get_pixel(px as u32, py as u32).0;
            let src = [
                color[0],
                color[1],
                color[2],
                (a * 255.0).round().clamp(0.0, 255.0) as u8,
            ];
            img.put_pixel(px as u32, py as u32, Rgba(over(dst, src)));
        }
    }
}

/// Deepest de Casteljau split `flatten_bezier` will follow. 2^16 segments out
/// of one control polygon is far past pixel precision; the cap is what keeps a
/// degenerate polygon (every point identical, or ends that meet with the middle
/// far away) bounded instead of recursing until the stack gives out.
const BEZIER_MAX_DEPTH: u32 = 16;

/// Flatten an arbitrary-degree Bezier control polygon (2 = line, 3 = quadratic,
/// 4 = cubic, …) to a polyline by recursive de Casteljau subdivision: split at
/// t=0.5 until every interior control point sits within `tolerance` pixels of
/// the endpoints' chord, then emit the chord. The polygon's own endpoints are
/// kept exactly — a curve must land where its ends were placed — and
/// consecutive duplicate vertices are collapsed. Fewer than 2 points is not a
/// curve and flattens to nothing.
pub fn flatten_bezier(points: &[(f32, f32)], tolerance: f32) -> Vec<(f32, f32)> {
    if points.len() < 2 {
        return Vec::new();
    }
    if points.len() == 2 {
        return points.to_vec();
    }
    let mut out = vec![points[0]];
    flatten_bezier_into(points, tolerance.max(0.0), 0, &mut out);
    out.dedup();
    out
}

/// Recursion core of `flatten_bezier`: appends only each accepted chord's LAST
/// endpoint, so neighbouring splits share a vertex without emitting it twice.
fn flatten_bezier_into(ctrl: &[(f32, f32)], tol: f32, depth: u32, out: &mut Vec<(f32, f32)>) {
    let (ax, ay) = ctrl[0];
    let last = ctrl[ctrl.len() - 1];
    let flat = depth >= BEZIER_MAX_DEPTH
        || ctrl[1..ctrl.len() - 1]
            .iter()
            .all(|&(px, py)| point_seg(px, py, ax, ay, last.0, last.1).0 <= tol);
    if flat {
        out.push(last);
        return;
    }
    // de Casteljau at t=0.5: each reduction row's first element extends the
    // left child polygon, its last element the (reversed) right one.
    let (mut left, mut right) = (
        Vec::with_capacity(ctrl.len()),
        Vec::with_capacity(ctrl.len()),
    );
    let mut row: Vec<(f32, f32)> = ctrl.to_vec();
    left.push(row[0]);
    right.push(row[row.len() - 1]);
    while row.len() > 1 {
        let mut next = Vec::with_capacity(row.len() - 1);
        for w in row.windows(2) {
            next.push(((w[0].0 + w[1].0) * 0.5, (w[0].1 + w[1].1) * 0.5));
        }
        left.push(next[0]);
        right.push(next[next.len() - 1]);
        row = next;
    }
    right.reverse();
    flatten_bezier_into(&left, tol, depth + 1, out);
    flatten_bezier_into(&right, tol, depth + 1, out);
}

// -- built-in 3×5 pixel font ------------------------------------------------

/// Glyph cell dimensions for the built-in font (3 wide × 5 tall).
pub const GLYPH_W: i32 = 3;
pub const GLYPH_H: i32 = 5;

/// Pack five 3-bit rows (top→bottom, each `0b___` with the left column as the
/// high bit) into one 15-bit glyph bitmask: bit `row*3 + col`, col 0 = left.
const fn g(r0: u16, r1: u16, r2: u16, r3: u16, r4: u16) -> u16 {
    r0 | (r1 << 3) | (r2 << 6) | (r3 << 9) | (r4 << 12)
}

/// Unknown characters render as this hollow 3×5 box.
const GLYPH_UNKNOWN: u16 = g(0b111, 0b101, 0b101, 0b101, 0b111);

/// Look up the 3×5 bitmask for a character. Lowercase maps to uppercase; any
/// glyph not in the set (covers A-Z, 0-9 and `. , : ! ? - + / ( ) '` and space)
/// returns the hollow box. See `GLYPH_W`/`GLYPH_H` for the cell size.
pub fn glyph(c: char) -> u16 {
    match c.to_ascii_uppercase() {
        ' ' => 0,
        'A' => g(0b010, 0b101, 0b111, 0b101, 0b101),
        'B' => g(0b110, 0b101, 0b110, 0b101, 0b110),
        'C' => g(0b011, 0b100, 0b100, 0b100, 0b011),
        'D' => g(0b110, 0b101, 0b101, 0b101, 0b110),
        'E' => g(0b111, 0b100, 0b110, 0b100, 0b111),
        'F' => g(0b111, 0b100, 0b110, 0b100, 0b100),
        'G' => g(0b011, 0b100, 0b101, 0b101, 0b011),
        'H' => g(0b101, 0b101, 0b111, 0b101, 0b101),
        'I' => g(0b111, 0b010, 0b010, 0b010, 0b111),
        'J' => g(0b001, 0b001, 0b001, 0b101, 0b010),
        'K' => g(0b101, 0b101, 0b110, 0b101, 0b101),
        'L' => g(0b100, 0b100, 0b100, 0b100, 0b111),
        'M' => g(0b101, 0b111, 0b111, 0b101, 0b101),
        'N' => g(0b101, 0b111, 0b111, 0b111, 0b101),
        'O' => g(0b010, 0b101, 0b101, 0b101, 0b010),
        'P' => g(0b110, 0b101, 0b110, 0b100, 0b100),
        'Q' => g(0b010, 0b101, 0b101, 0b110, 0b011),
        'R' => g(0b110, 0b101, 0b110, 0b101, 0b101),
        'S' => g(0b011, 0b100, 0b010, 0b001, 0b110),
        'T' => g(0b111, 0b010, 0b010, 0b010, 0b010),
        'U' => g(0b101, 0b101, 0b101, 0b101, 0b010),
        'V' => g(0b101, 0b101, 0b101, 0b010, 0b010),
        'W' => g(0b101, 0b101, 0b111, 0b111, 0b101),
        'X' => g(0b101, 0b101, 0b010, 0b101, 0b101),
        'Y' => g(0b101, 0b101, 0b010, 0b010, 0b010),
        'Z' => g(0b111, 0b001, 0b010, 0b100, 0b111),
        '0' => g(0b010, 0b101, 0b101, 0b101, 0b010),
        '1' => g(0b010, 0b110, 0b010, 0b010, 0b111),
        '2' => g(0b110, 0b001, 0b010, 0b100, 0b111),
        '3' => g(0b110, 0b001, 0b010, 0b001, 0b110),
        '4' => g(0b101, 0b101, 0b111, 0b001, 0b001),
        '5' => g(0b111, 0b100, 0b110, 0b001, 0b110),
        '6' => g(0b011, 0b100, 0b110, 0b101, 0b010),
        '7' => g(0b111, 0b001, 0b010, 0b010, 0b010),
        '8' => g(0b010, 0b101, 0b010, 0b101, 0b010),
        '9' => g(0b010, 0b101, 0b011, 0b001, 0b110),
        '.' => g(0b000, 0b000, 0b000, 0b000, 0b010),
        ',' => g(0b000, 0b000, 0b000, 0b010, 0b100),
        ':' => g(0b000, 0b010, 0b000, 0b010, 0b000),
        '!' => g(0b010, 0b010, 0b010, 0b000, 0b010),
        '?' => g(0b110, 0b001, 0b010, 0b000, 0b010),
        '-' => g(0b000, 0b000, 0b111, 0b000, 0b000),
        '+' => g(0b000, 0b010, 0b111, 0b010, 0b000),
        '/' => g(0b001, 0b001, 0b010, 0b100, 0b100),
        '(' => g(0b001, 0b010, 0b010, 0b010, 0b001),
        ')' => g(0b100, 0b010, 0b010, 0b010, 0b100),
        '\'' => g(0b010, 0b010, 0b000, 0b000, 0b000),
        _ => GLYPH_UNKNOWN,
    }
}

/// Resolve an optional inclusive region against a w×h canvas: clamp to
/// bounds (normalising reversed corners), default to the full canvas. A
/// region left empty by clamping is a caller mistake and errors loudly —
/// the one policy every region-taking op shares.
pub fn resolve_region(
    region: Option<(i32, i32, i32, i32)>,
    w: u32,
    h: u32,
) -> Result<(i32, i32, i32, i32), String> {
    match region {
        Some((x0, y0, x1, y1)) => clamp_region(x0, y0, x1, y1, w, h)
            .ok_or_else(|| "region is empty after clamping to the canvas".to_string()),
        None => Ok((0, 0, w as i32 - 1, h as i32 - 1)),
    }
}

/// Normalise a possibly-reversed rect and clamp it to a `w`×`h` canvas, returning
/// `(x0,y0,x1,y1)` (inclusive) or `None` when it lies fully outside the canvas.
pub fn clamp_region(
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    w: u32,
    h: u32,
) -> Option<(i32, i32, i32, i32)> {
    let (lx, hx) = (x0.min(x1), x0.max(x1));
    let (ly, hy) = (y0.min(y1), y0.max(y1));
    if hx < 0 || hy < 0 || lx >= w as i32 || ly >= h as i32 {
        return None;
    }
    Some((
        lx.max(0),
        ly.max(0),
        hx.min(w as i32 - 1),
        hy.min(h as i32 - 1),
    ))
}

/// Separable layer blend modes (W3C Compositing & Blending). `Normal` reduces
/// the compositor to plain source-over, so existing art renders unchanged.
#[derive(Clone, Copy, PartialEq)]
pub enum Blend {
    Normal,
    Multiply,
    Screen,
    Add,
    Subtract,
    Darken,
    Lighten,
    Difference,
    Exclusion,
    Overlay,
    HardLight,
    SoftLight,
    ColorDodge,
    ColorBurn,
}

/// Parse a canonical blend-mode name. Unknown values are never coerced.
pub(crate) fn parse_blend(s: &str) -> Option<Blend> {
    Some(match s {
        "normal" => Blend::Normal,
        "multiply" => Blend::Multiply,
        "screen" => Blend::Screen,
        "add" => Blend::Add,
        "subtract" => Blend::Subtract,
        "darken" => Blend::Darken,
        "lighten" => Blend::Lighten,
        "difference" => Blend::Difference,
        "exclusion" => Blend::Exclusion,
        "overlay" => Blend::Overlay,
        "hard-light" => Blend::HardLight,
        "soft-light" => Blend::SoftLight,
        "color-dodge" => Blend::ColorDodge,
        "color-burn" => Blend::ColorBurn,
        _ => return None,
    })
}

/// Human-readable list of accepted blend tokens (for validation error messages).
pub const BLEND_NAMES: &str = "normal | multiply | screen | add | subtract | darken | \
lighten | difference | exclusion | overlay | hard-light | soft-light | color-dodge | color-burn";

/// Whether `s` is a canonical blend token.
pub fn valid_blend(s: &str) -> bool {
    parse_blend(s).is_some()
}

/// `overlay(cb, cs)` — multiply on the dark half of the backdrop, screen on the
/// light half. `hard-light` is the same with the operands swapped.
fn overlay(cb: f32, cs: f32) -> f32 {
    if cb <= 0.5 {
        2.0 * cb * cs
    } else {
        1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
    }
}

fn soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

/// The separable blend function `B(Cb, Cs)` for one channel, all in [0,1].
fn blend_fn(mode: Blend, cb: f32, cs: f32) -> f32 {
    match mode {
        Blend::Normal => cs,
        Blend::Multiply => cb * cs,
        Blend::Screen => cb + cs - cb * cs,
        Blend::Add => (cb + cs).min(1.0),
        Blend::Subtract => (cb - cs).max(0.0),
        Blend::Darken => cb.min(cs),
        Blend::Lighten => cb.max(cs),
        Blend::Difference => (cb - cs).abs(),
        Blend::Exclusion => cb + cs - 2.0 * cb * cs,
        Blend::Overlay => overlay(cb, cs),
        Blend::HardLight => overlay(cs, cb),
        Blend::SoftLight => soft_light(cb, cs),
        Blend::ColorDodge => {
            if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        Blend::ColorBurn => {
            if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
    }
}

/// Composite one `src` pixel over one `dst` pixel, scaling source alpha by `of`
/// (0..1) and mixing colour through blend mode `mode`. The W3C formula
/// `Cs' = (1-αb)·Cs + αb·B(Cb,Cs)` then source-over: `Normal` is plain straight-
/// alpha source-over, and a fully-transparent backdrop ignores the blend (a
/// multiply pixel over nothing keeps its own colour, not black).
pub fn composite_px(d: [u8; 4], s: [u8; 4], of: f32, mode: Blend) -> [u8; 4] {
    let sa = (s[3] as f32 / 255.0) * of;
    if sa <= 0.0 {
        return d;
    }
    let da = d[3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return [0, 0, 0, 0];
    }
    let ch = |i: usize| -> u8 {
        let cs = s[i] as f32 / 255.0;
        let cb = d[i] as f32 / 255.0;
        let csb = (1.0 - da) * cs + da * blend_fn(mode, cb, cs);
        ((sa * csb + da * (1.0 - sa) * cb) / out_a * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [ch(0), ch(1), ch(2), (out_a * 255.0).round() as u8]
}

/// Composite `src` onto `dst` at (ox, oy), scaling src alpha by `opacity` and
/// mixing through blend mode `mode`. `Normal` is exactly the old straight-alpha
/// source-over.
pub fn composite(dst: &mut RgbaImage, src: &RgbaImage, ox: i32, oy: i32, opacity: u8, mode: Blend) {
    let (dw, dh) = (dst.width() as i32, dst.height() as i32);
    let of = opacity as f32 / 255.0;
    for y in 0..src.height() as i32 {
        let ty = oy + y;
        if ty < 0 || ty >= dh {
            continue;
        }
        for x in 0..src.width() as i32 {
            let tx = ox + x;
            if tx < 0 || tx >= dw {
                continue;
            }
            let s = src.get_pixel(x as u32, y as u32).0;
            if s[3] == 0 {
                continue;
            }
            let d = dst.get_pixel(tx as u32, ty as u32).0;
            dst.put_pixel(tx as u32, ty as u32, Rgba(composite_px(d, s, of, mode)));
        }
    }
}

/// Separable box blur of radius `r` (premultiplied alpha, so transparent regions
/// don't bleed dark haloes). Shared by blur / drop-shadow / glow.
pub fn box_blur(src: &RgbaImage, r: i32) -> RgbaImage {
    if r <= 0 {
        return src.clone();
    }
    let (w, h) = (src.width() as i32, src.height() as i32);
    // A radius beyond w+h already covers every pixel from every centre, so
    // clamp rather than pay for it: `2*r+1` overflowed i32 near i32::MAX, and
    // the window-seeding loop below is O(r) per line.
    let r = r.min(w + h);
    // To premultiplied f32 buffers.
    let n = (w * h) as usize;
    let (mut pr, mut pg, mut pb, mut pa) =
        (vec![0f32; n], vec![0f32; n], vec![0f32; n], vec![0f32; n]);
    for y in 0..h {
        for x in 0..w {
            let p = src.get_pixel(x as u32, y as u32).0;
            let a = p[3] as f32 / 255.0;
            let i = (y * w + x) as usize;
            pr[i] = p[0] as f32 * a;
            pg[i] = p[1] as f32 * a;
            pb[i] = p[2] as f32 * a;
            pa[i] = a;
        }
    }
    // Sliding-window pass: O(w·h) per axis regardless of radius (the naive
    // per-pixel window made glow/shadow cost scale linearly with r).
    let blur1 = |buf: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0f32; n];
        let win = (2 * r + 1) as f32;
        let (lines, len) = if horizontal { (h, w) } else { (w, h) };
        let at = |line: i32, i: i32| -> usize {
            let i = i.clamp(0, len - 1);
            if horizontal {
                (line * w + i) as usize
            } else {
                (i * w + line) as usize
            }
        };
        for line in 0..lines {
            let mut acc = 0.0;
            for k in -r..=r {
                acc += buf[at(line, k)];
            }
            out[at(line, 0)] = acc / win;
            for i in 1..len {
                acc += buf[at(line, i + r)] - buf[at(line, i - r - 1)];
                out[at(line, i)] = acc / win;
            }
        }
        out
    };
    for buf in [&mut pr, &mut pg, &mut pb, &mut pa] {
        let tmp = blur1(buf, true);
        *buf = blur1(&tmp, false);
    }
    let mut out = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let a = pa[i];
            let px = if a > 0.0 {
                [
                    (pr[i] / a).round().clamp(0.0, 255.0) as u8,
                    (pg[i] / a).round().clamp(0.0, 255.0) as u8,
                    (pb[i] / a).round().clamp(0.0, 255.0) as u8,
                    (a * 255.0).round().clamp(0.0, 255.0) as u8,
                ]
            } else {
                [0, 0, 0, 0]
            };
            out.put_pixel(x as u32, y as u32, Rgba(px));
        }
    }
    out
}

/// Straight-alpha source-over of one `src` pixel onto one `dst` pixel (the
/// per-pixel painting path used by gradient/scatter). Equivalent to `composite`
/// with `Blend::Normal` for a single pixel.
pub fn over(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let sa = src[3] as f32 / 255.0;
    if sa >= 1.0 {
        return src;
    }
    if sa <= 0.0 {
        return dst;
    }
    let da = dst[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return [0, 0, 0, 0];
    }
    let ch = |i: usize| {
        (((src[i] as f32 * sa + dst[i] as f32 * da * (1.0 - sa)) / oa)
            .round()
            .clamp(0.0, 255.0)) as u8
    };
    [ch(0), ch(1), ch(2), (oa * 255.0).round() as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_modes_accept_only_the_documented_names() {
        for name in BLEND_NAMES.split(" | ") {
            assert!(valid_blend(name), "canonical blend rejected: {name}");
        }
        for alias in [
            "additive",
            "linear-dodge",
            "linear_dodge",
            "hard_light",
            "hardlight",
            "soft_light",
            "softlight",
            "color_dodge",
            "dodge",
            "color_burn",
            "burn",
        ] {
            assert!(!valid_blend(alias), "undocumented alias accepted: {alias}");
        }
    }

    #[test]
    fn median_cut_weighted_pins_consume_the_budget() {
        let pixels: Vec<([u8; 3], u64)> = vec![([10, 20, 30], 100), ([200, 50, 50], 5)];
        let pins = [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
        ];
        // Pins fill the budget exactly: no bonus derived colour sneaks past n.
        let pal = median_cut_weighted(&pixels, 4, &pins);
        assert_eq!(pal.len(), 4);
        assert_eq!(pal, pins.to_vec());
        // With headroom, derived colours fill the remainder.
        let pal6 = median_cut_weighted(&pixels, 6, &pins);
        assert!(pal6.len() > 4 && pal6.len() <= 6);
    }

    #[test]
    fn oklab_round_trips_within_one_step() {
        for c in [
            [10, 20, 30, 255],
            [200, 120, 40, 255],
            [255, 255, 255, 255],
            [0, 128, 64, 255],
        ] {
            let back = oklab_to_srgb(srgb_to_oklab(c));
            for i in 0..3 {
                assert!(
                    (back[i] as i32 - c[i] as i32).abs() <= 1,
                    "{:?} -> {:?}",
                    c,
                    back
                );
            }
        }
    }

    #[test]
    fn close_rgb_ignores_alpha_and_uses_max_channel() {
        assert!(close_rgb([200, 0, 0, 255], [200, 0, 0, 10], 0)); // same RGB, diff alpha
        assert!(close_rgb([200, 0, 0, 255], [205, 3, 0, 255], 5)); // within 5 per channel
        assert!(!close_rgb([200, 0, 0, 255], [180, 0, 0, 255], 5)); // R off by 20 > 5
    }

    #[test]
    fn ramp_gamut_maps_without_hue_shift() {
        // A vivid red at full chroma across the lightness range would, without
        // gamut mapping, per-channel clamp on the bright steps and skew red→orange.
        // With chroma reduced to fit, every chromatic step keeps the base hue.
        let base = [220, 30, 40, 255];
        let (_, _, base_h) = oklab_to_oklch(srgb_to_oklab(base));
        let ramp = make_ramp_oklch(base, 6, 0.2, 0.92, 0.0, "flat", false);
        for c in &ramp {
            let (_, cc, hh) = oklab_to_oklch(srgb_to_oklab(*c));
            if cc < 0.03 {
                continue; // achromatic — hue is meaningless
            }
            let dh = ((hh - base_h + 540.0).rem_euclid(360.0) - 180.0).abs();
            assert!(
                dh < 12.0,
                "step {c:?} hue {hh:.0}° drifted {dh:.0}° from base {base_h:.0}°"
            );
        }
    }

    #[test]
    fn perceptual_ramp_is_monotonic_and_anchors_midtone() {
        let base = [120, 80, 60, 255];
        let ramp = make_ramp_oklch(base, 5, 0.2, 0.85, 25.0, "arc", true);
        assert_eq!(ramp.len(), 5);
        // perceptual lightness rises across the ramp
        let ls: Vec<f32> = ramp.iter().map(|c| srgb_to_oklab(*c).0).collect();
        for w in ls.windows(2) {
            assert!(w[1] >= w[0] - 0.001, "not monotonic: {:?}", ls);
        }
        // midtone anchored to base
        assert_eq!(ramp[2], base);
    }

    #[test]
    fn nearest_oklab_beats_naive_rgb_on_a_hue() {
        // a desaturated teal is perceptually nearer teal than near-equal-RGB grey
        let i = nearest_oklab(
            [60, 120, 120, 255],
            &[[128, 128, 128, 255], [40, 150, 150, 255]],
        );
        assert_eq!(i, Some(1));
    }

    #[test]
    fn glyph_lowercase_maps_to_uppercase_and_unknown_is_box() {
        assert_eq!(glyph('a'), glyph('A'));
        assert_eq!(glyph(' '), 0);
        // A character outside the set falls back to the hollow box.
        assert_eq!(glyph('@'), GLYPH_UNKNOWN);
        // The box is a 3×5 ring: corners on, centre off. Left column is the
        // high bit of each 3-bit row, so col x reads bit (GLYPH_W-1-x).
        let bit = |x: i32, y: i32| (GLYPH_UNKNOWN >> (y * GLYPH_W + (GLYPH_W - 1 - x))) & 1 == 1;
        assert!(bit(0, 0) && bit(2, 0) && bit(0, 4) && bit(2, 4));
        assert!(!bit(1, 2)); // hollow centre
    }

    #[test]
    fn ramp_runs_dark_to_light() {
        let r = make_ramp([120, 80, 60, 255], 5, 20.0, 0.35, 0.1);
        assert_eq!(r.len(), 5);
        let sum3 = |c: [u8; 4]| c[0] as i32 + c[1] as i32 + c[2] as i32;
        assert!(sum3(r[0]) < sum3(r[4]), "ramp should brighten");
    }

    #[test]
    fn hsl_round_trip() {
        for &c in &[
            [200u8, 80, 40],
            [40, 160, 90],
            [60, 70, 200],
            [128, 128, 128],
        ] {
            let (h, s, l) = rgb_to_hsl(c[0], c[1], c[2]);
            let back = hsl_to_rgb(h, s, l);
            for i in 0..3 {
                assert!(
                    (back[i] as i32 - c[i] as i32).abs() <= 1,
                    "channel {i}: {} vs {}",
                    back[i],
                    c[i]
                );
            }
        }
    }

    #[test]
    fn composite_multiply_over_empty_keeps_source() {
        // A multiply pixel over a fully-transparent backdrop must keep its own
        // colour, not go black.
        let out = composite_px([0, 0, 0, 0], [200, 120, 40, 255], 1.0, Blend::Multiply);
        assert_eq!(out, [200, 120, 40, 255]);
    }

    #[test]
    fn dither_checker_alternates() {
        // Checker is a 1-bit board: (x+y) even → 0.25, odd → 0.75.
        assert_eq!(dither_threshold("checker", 0, 0), 0.25);
        assert_eq!(dither_threshold("checker", 1, 0), 0.75);
        assert_eq!(dither_threshold("checker", 1, 1), 0.25);
    }

    #[test]
    fn median_cut_reduces_palette_to_n() {
        let mut pixels = Vec::new();
        for r in 0..8u8 {
            for g in 0..8u8 {
                pixels.push([r * 32, g * 32, 0]);
            }
        }
        let pal = median_cut(&pixels, 4);
        assert_eq!(pal.len(), 4);
        assert!(pal.iter().all(|c| c[3] == 255));
    }

    #[test]
    fn clamp_region_normal_reversed_outside() {
        // Normal rect, fully inside, unchanged.
        assert_eq!(clamp_region(1, 1, 3, 3, 10, 10), Some((1, 1, 3, 3)));
        // Reversed corners are normalised and clamped to the canvas.
        assert_eq!(clamp_region(8, 8, 2, 2, 5, 5), Some((2, 2, 4, 4)));
        // Fully outside → None.
        assert_eq!(clamp_region(20, 20, 30, 30, 5, 5), None);
    }

    #[test]
    fn flatten_bezier_two_points_is_the_straight_segment() {
        let seg = [(1.0, 2.0), (5.0, 6.0)];
        assert_eq!(flatten_bezier(&seg, 0.25), seg.to_vec());
        // Fewer than two points is not a curve: nothing to flatten to.
        assert!(flatten_bezier(&[], 0.25).is_empty());
        assert!(flatten_bezier(&[(1.0, 2.0)], 0.25).is_empty());
    }

    #[test]
    fn flatten_bezier_keeps_the_endpoints_exact() {
        let cubic = [(0.0, 0.0), (10.0, 20.0), (30.0, 20.0), (40.0, 0.0)];
        let flat = flatten_bezier(&cubic, 0.25);
        assert!(flat.len() > 2, "a bent cubic must subdivide, not collapse");
        assert_eq!(flat.first(), Some(&cubic[0]));
        assert_eq!(flat.last(), Some(&cubic[3]));
    }

    #[test]
    fn flatten_bezier_quadratic_bows_toward_the_control_point() {
        // Control point pulled 8px above the chord: the flattened apex must
        // leave the chord on the control point's side — nearer the control
        // point than the chord's own midpoint is.
        let quad = [(0.0, 8.0), (8.0, 0.0), (16.0, 8.0)];
        let flat = flatten_bezier(&quad, 0.25);
        let apex = flat[flat.len() / 2];
        let dist = |(x0, y0): (f32, f32), (x1, y1): (f32, f32)| {
            ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
        };
        let chord_mid = (8.0, 8.0);
        assert!(
            dist(apex, quad[1]) < dist(chord_mid, quad[1]),
            "apex {apex:?} did not move toward the control point"
        );
        // And it must sit well off the chord — a straight line would score 0.
        assert!(point_seg(apex.0, apex.1, 0.0, 8.0, 16.0, 8.0).0 > 1.0);
    }

    #[test]
    fn flatten_bezier_degenerate_polygons_terminate() {
        // Collinear polygon: already flat against its own chord.
        assert_eq!(
            flatten_bezier(&[(0.0, 0.0), (5.0, 5.0), (9.0, 9.0)], 0.25),
            vec![(0.0, 0.0), (9.0, 9.0)]
        );
        // Every point identical: collapses to a single dot.
        assert_eq!(flatten_bezier(&[(3.0, 3.0); 5], 0.25), vec![(3.0, 3.0)]);
        // Ends that meet with the middle far away subdivide until flat —
        // bounded by the depth cap, never a hang.
        let looped = flatten_bezier(&[(0.0, 0.0), (100.0, 100.0), (0.0, 0.0)], 0.25);
        assert!(looped.len() <= (1usize << BEZIER_MAX_DEPTH) + 1);
        assert_eq!(looped.first(), Some(&(0.0, 0.0)));
        assert_eq!(looped.last(), Some(&(0.0, 0.0)));
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    #[test]
    fn median_cut_weighted_is_input_order_independent() {
        // Callers build the pair list from HashMaps (random iteration order);
        // the palette must come out identical regardless.
        let a: Vec<([u8; 3], u64)> = vec![
            ([10, 20, 30], 50),
            ([200, 50, 50], 5),
            ([40, 200, 60], 20),
            ([90, 90, 90], 33),
            ([250, 240, 230], 8),
        ];
        let mut b = a.clone();
        b.reverse();
        let mut c = a.clone();
        c.rotate_left(2);
        let pa = median_cut_weighted(&a, 4, &[]);
        assert_eq!(pa, median_cut_weighted(&b, 4, &[]));
        assert_eq!(pa, median_cut_weighted(&c, 4, &[]));
    }

    #[test]
    fn draw_line_clips_a_continental_span_to_the_canvas() {
        let mut img = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0]));
        // Both endpoints far off-canvas but the segment crosses it.
        draw_line(
            &mut img,
            -1_000_000_000,
            4,
            1_000_000_000,
            4,
            [9, 9, 9, 255],
            1,
        );
        for x in 0..8 {
            assert_eq!(img.get_pixel(x, 4).0, [9, 9, 9, 255], "x={x}");
        }
        // Fully outside: nothing drawn (and no multi-billion-step walk).
        let mut img2 = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0]));
        draw_line(
            &mut img2,
            1_000_000_000,
            0,
            1_000_000_001,
            7,
            [9, 9, 9, 255],
            1,
        );
        assert!(img2.pixels().all(|p| p.0[3] == 0));
        // i32 extremes no longer overflow the span math.
        let mut img3 = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0]));
        draw_line(&mut img3, i32::MIN, 0, i32::MAX, 7, [9, 9, 9, 255], 1);
        assert!(img3.pixels().any(|p| p.0[3] > 0));
    }
}
