//! Pure pixel / colour / noise functions with no `Document` knowledge.
//!
//! Free functions over `RgbaImage` and `[u8; 4]` colours: drawing primitives,
//! blend/composite, colour-space conversion, procedural noise, palette
//! quantisation and shading ramps. Everything here is stateless and reusable.

use image::{Rgba, RgbaImage};

// -- drawing helpers (overwrite semantics; alpha 0 = erase) -----------------

/// Overwrite a single pixel if (x,y) is inside the image bounds.
pub fn put(img: &mut RgbaImage, x: i32, y: i32, color: [u8; 4]) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, Rgba(color));
    }
}

/// Stamp a `size`×`size` square brush centred at (cx, cy).
pub fn brush(img: &mut RgbaImage, cx: i32, cy: i32, color: [u8; 4], size: i32) {
    let o = size / 2;
    for dy in 0..size {
        for dx in 0..size {
            put(img, cx - o + dx, cy - o + dy, color);
        }
    }
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

/// Manhattan colour distance over all 4 channels within tolerance.
pub fn close(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    let d: i32 = (0..4).map(|i| (a[i] as i32 - b[i] as i32).abs()).sum();
    d <= tol
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

/// Perceptual luma L = 0.2126R + 0.7152G + 0.0722B on 0..255 (rounded). The one
/// "value"/brightness definition shared by every analysis tool. Alpha-agnostic.
pub fn luma(c: [u8; 4]) -> u8 {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32).round() as u8
}

/// HSL hue (degrees) of an RGBA colour — the public read of `rgb_to_hsl`'s H.
pub fn hue_deg(c: [u8; 4]) -> f32 {
    rgb_to_hsl(c[0], c[1], c[2]).0
}

/// HSL saturation (0..1) of an RGBA colour.
pub fn saturation(c: [u8; 4]) -> f32 {
    rgb_to_hsl(c[0], c[1], c[2]).1
}

/// WCAG relative luminance (0..1) from linearised sRGB. The `(L+0.05)` form fed
/// into the contrast ratio — distinct from perceptual `luma`, which is gamma
/// space. Alpha-agnostic.
fn wcag_luminance(c: [u8; 4]) -> f32 {
    let lin = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// WCAG contrast ratio (1..21) between two colours: (Llighter+0.05)/(Ldarker+0.05).
pub fn wcag_ratio(a: [u8; 4], b: [u8; 4]) -> f32 {
    let (la, lb) = (wcag_luminance(a), wcag_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
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

/// Parse a blend-mode name (with hyphen/underscore aliases); unknown → `Normal`.
pub fn parse_blend(s: &str) -> Blend {
    match s {
        "multiply" => Blend::Multiply,
        "screen" => Blend::Screen,
        "add" | "additive" | "linear-dodge" | "linear_dodge" => Blend::Add,
        "subtract" => Blend::Subtract,
        "darken" => Blend::Darken,
        "lighten" => Blend::Lighten,
        "difference" => Blend::Difference,
        "exclusion" => Blend::Exclusion,
        "overlay" => Blend::Overlay,
        "hard-light" | "hard_light" | "hardlight" => Blend::HardLight,
        "soft-light" | "soft_light" | "softlight" => Blend::SoftLight,
        "color-dodge" | "color_dodge" | "dodge" => Blend::ColorDodge,
        "color-burn" | "color_burn" | "burn" => Blend::ColorBurn,
        _ => Blend::Normal,
    }
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
    let blur1 = |buf: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0f32; n];
        let win = (2 * r + 1) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for k in -r..=r {
                    let (sx, sy) = if horizontal { (x + k, y) } else { (x, y + k) };
                    let cx = sx.clamp(0, w - 1);
                    let cy = sy.clamp(0, h - 1);
                    acc += buf[(cy * w + cx) as usize];
                }
                out[(y * w + x) as usize] = acc / win;
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

/// RGB (0..255) → HSL with h in degrees [0,360), s and l in [0,1].
pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h.rem_euclid(360.0), s, l)
}

/// HSL (h degrees, s/l in [0,1]) → RGB (0..255).
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// Rotate `src` by `deg` (clockwise) about its centre with nearest-neighbour
/// sampling, returning a new image sized to the rotated bounding box.
pub fn rotate_nn(src: &RgbaImage, deg: f32) -> RgbaImage {
    let rad = deg.to_radians();
    let (c, s) = (rad.cos(), rad.sin());
    let (w, h) = (src.width() as f32, src.height() as f32);
    let rot = |x: f32, y: f32| (x * c - y * s, x * s + y * c);
    let corners = [rot(0.0, 0.0), rot(w, 0.0), rot(0.0, h), rot(w, h)];
    let minx = corners.iter().map(|p| p.0).fold(f32::MAX, f32::min);
    let maxx = corners.iter().map(|p| p.0).fold(f32::MIN, f32::max);
    let miny = corners.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let maxy = corners.iter().map(|p| p.1).fold(f32::MIN, f32::max);
    let (nw, nh) = (
        ((maxx - minx).ceil() as u32).max(1),
        ((maxy - miny).ceil() as u32).max(1),
    );
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (ncx, ncy) = (nw as f32 / 2.0, nh as f32 / 2.0);
    let mut out = RgbaImage::from_pixel(nw, nh, Rgba([0, 0, 0, 0]));
    for oy in 0..nh {
        for ox in 0..nw {
            let (dx, dy) = (ox as f32 - ncx, oy as f32 - ncy);
            // inverse rotation (about centre)
            let sx = dx * c + dy * s + cx;
            let sy = -dx * s + dy * c + cy;
            if sx >= 0.0 && sy >= 0.0 && (sx as u32) < src.width() && (sy as u32) < src.height() {
                let p = *src.get_pixel(sx as u32, sy as u32);
                if p.0[3] > 0 {
                    out.put_pixel(ox, oy, p);
                }
            }
        }
    }
    out
}

/// Linear interpolation between `a` and `b` by `t`.
pub fn lerpf(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Cubic easing of a 0..1 progress `t`. "ease-in" t³ (slow start), "ease-out"
/// the mirror (slow end), "ease-in-out" the symmetric blend, anything else
/// linear. Used by keyframe motion so a tween accelerates/decelerates.
pub fn ease(t: f32, kind: &str) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match kind {
        "ease-in" => t * t * t,
        "ease-out" => {
            let u = 1.0 - t;
            1.0 - u * u * u
        }
        "ease-in-out" => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                let u = -2.0 * t + 2.0;
                1.0 - u * u * u / 2.0
            }
        }
        _ => t, // "linear" and any unknown easing
    }
}

/// Quintic smootherstep fade for noise interpolation.
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// A lattice random in [0,1] at integer cell (ix,iy).
fn vrand(ix: i32, iy: i32, seed: u64) -> f32 {
    hash2(ix, iy, seed) as f32 / u32::MAX as f32
}

/// Smooth value noise at (x,y) (faded bilinear of lattice randoms) → [0,1].
fn value_noise(x: f32, y: f32, seed: u64) -> f32 {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    let (u, v) = (fade(fx), fade(fy));
    let a = vrand(ix, iy, seed);
    let b = vrand(ix + 1, iy, seed);
    let c = vrand(ix, iy + 1, seed);
    let d = vrand(ix + 1, iy + 1, seed);
    lerpf(lerpf(a, b, u), lerpf(c, d, u), v)
}

/// Fractal (fBm) value noise — summed octaves → soft clouds, in [0,1].
pub fn fbm(x: f32, y: f32, seed: u64, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 0.5, 1.0, 0.0);
    for o in 0..octaves.max(1) {
        sum += amp * value_noise(x * freq, y * freq, seed.wrapping_add(o as u64 * 1311));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

/// Perlin gradient noise at (x,y) → [0,1].
pub fn perlin(x: f32, y: f32, seed: u64) -> f32 {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let grad = |cx: i32, cy: i32| {
        let a = vrand(cx, cy, seed) * std::f32::consts::TAU;
        (a.cos(), a.sin())
    };
    let dot = |cx: i32, cy: i32| {
        let (gx, gy) = grad(cx, cy);
        (x - cx as f32) * gx + (y - cy as f32) * gy
    };
    let (u, v) = (fade(x - ix as f32), fade(y - iy as f32));
    let a = lerpf(dot(ix, iy), dot(ix + 1, iy), u);
    let b = lerpf(dot(ix, iy + 1), dot(ix + 1, iy + 1), u);
    (lerpf(a, b, v) * 0.7 + 0.5).clamp(0.0, 1.0)
}

/// Worley/Voronoi cellular noise: distance to the nearest feature point → [0,1].
pub fn voronoi(x: f32, y: f32, seed: u64) -> f32 {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let mut md = f32::MAX;
    for oy in -1..=1 {
        for ox in -1..=1 {
            let (cx, cy) = (ix + ox, iy + oy);
            let fx = cx as f32 + vrand(cx, cy, seed);
            let fy = cy as f32 + vrand(cx, cy, seed.wrapping_add(0x9999));
            md = md.min(((x - fx).powi(2) + (y - fy).powi(2)).sqrt());
        }
    }
    md.clamp(0.0, 1.0)
}

/// Deterministic, seedable per-pixel hash → u32 (integer-mix; no float/RNG state
/// so scatter and noise dithering reproduce exactly for a given seed).
pub fn hash2(x: i32, y: i32, seed: u64) -> u32 {
    let mut h = seed ^ 0x9E37_79B9_7F4A_7C15;
    h = h.wrapping_add((x as u32 as u64).wrapping_mul(0x85EB_CA77_C2B2_AE63));
    h ^= h >> 29;
    h = h.wrapping_add((y as u32 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F));
    h ^= h >> 32;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h as u32
}

/// Median-cut colour quantisation: reduce `pixels` (opaque RGB) to at most `n`
/// representative colours by recursively splitting the colour box along its
/// longest axis at the median, then averaging each box.
pub fn median_cut(pixels: &[[u8; 3]], n: usize) -> Vec<[u8; 4]> {
    if pixels.is_empty() {
        return vec![[0, 0, 0, 255]];
    }
    let n = n.max(1);
    let mut boxes: Vec<Vec<[u8; 3]>> = vec![pixels.to_vec()];
    while boxes.len() < n {
        // Pick the splittable box with the widest channel range.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .max_by_key(|(_, b)| {
                (0..3)
                    .map(|c| {
                        let (mn, mx) = b
                            .iter()
                            .fold((255u8, 0u8), |(mn, mx), p| (mn.min(p[c]), mx.max(p[c])));
                        mx - mn
                    })
                    .max()
                    .unwrap_or(0)
            });
        let Some((bi, _)) = pick else { break };
        // Longest axis of that box.
        let axis = (0..3)
            .max_by_key(|&c| {
                let (mn, mx) = boxes[bi]
                    .iter()
                    .fold((255u8, 0u8), |(mn, mx), p| (mn.min(p[c]), mx.max(p[c])));
                mx - mn
            })
            .unwrap();
        boxes[bi].sort_by_key(|p| p[axis]);
        let mid = boxes[bi].len() / 2;
        let hi = boxes[bi].split_off(mid);
        boxes.push(hi);
    }
    boxes
        .iter()
        .map(|b| {
            let (mut r, mut g, mut bl) = (0u64, 0u64, 0u64);
            for p in b {
                r += p[0] as u64;
                g += p[1] as u64;
                bl += p[2] as u64;
            }
            let len = b.len().max(1) as u64;
            [(r / len) as u8, (g / len) as u8, (bl / len) as u8, 255]
        })
        .collect()
}

/// Generate a hue-shifted shading ramp from a base colour, darkest → lightest.
/// Lighter steps shift hue by `+hue_shift`° (toward warm) and lower saturation;
/// darker steps shift `-hue_shift`° (toward cool) and gain saturation — the
/// classic pixel-art ramp. `light_range` is the half-spread in lightness.
pub fn make_ramp(
    base: [u8; 4],
    count: usize,
    hue_shift: f32,
    light_range: f32,
    sat_shift: f32,
) -> Vec<[u8; 4]> {
    let count = count.max(1);
    let (h, s, l) = rgb_to_hsl(base[0], base[1], base[2]);
    (0..count)
        .map(|i| {
            let t = if count == 1 {
                0.5
            } else {
                i as f32 / (count - 1) as f32
            };
            let c = t - 0.5; // -0.5 (dark) .. +0.5 (light)
            let rgb = hsl_to_rgb(
                h + c * hue_shift,
                (s - c * sat_shift).clamp(0.0, 1.0),
                (l + c * 2.0 * light_range).clamp(0.0, 1.0),
            );
            [rgb[0], rgb[1], rgb[2], base[3]]
        })
        .collect()
}

/// 8×8 ordered Bayer threshold matrix → a value in [0,1) at pixel (x,y),
/// tiling across the canvas. Used to dither a gradient between two stop colours.
fn bayer8(x: i32, y: i32) -> f32 {
    const M: [[u8; 8]; 8] = [
        [0, 32, 8, 40, 2, 34, 10, 42],
        [48, 16, 56, 24, 50, 18, 58, 26],
        [12, 44, 4, 36, 14, 46, 6, 38],
        [60, 28, 52, 20, 62, 30, 54, 22],
        [3, 35, 11, 43, 1, 33, 9, 41],
        [51, 19, 59, 27, 49, 17, 57, 25],
        [15, 47, 7, 39, 13, 45, 5, 37],
        [63, 31, 55, 23, 61, 29, 53, 21],
    ];
    (M[(y.rem_euclid(8)) as usize][(x.rem_euclid(8)) as usize] as f32 + 0.5) / 64.0
}

/// Ordered-dither threshold in [0,1) at pixel (x,y) for a dither `pattern`.
/// "checker" is a 1-bit chequerboard (0.25/0.75); the bayer variants step up
/// the matrix size for finer ramps. Tiles across the canvas like `bayer8`.
pub fn dither_threshold(pattern: &str, x: i32, y: i32) -> f32 {
    match pattern {
        "checker" => {
            if (x + y).rem_euclid(2) == 0 {
                0.25
            } else {
                0.75
            }
        }
        "bayer2" => {
            const M: [[u8; 2]; 2] = [[0, 2], [3, 1]];
            (M[(y.rem_euclid(2)) as usize][(x.rem_euclid(2)) as usize] as f32 + 0.5) / 4.0
        }
        "bayer4" => {
            const M: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
            (M[(y.rem_euclid(4)) as usize][(x.rem_euclid(4)) as usize] as f32 + 0.5) / 16.0
        }
        // "bayer8" and any unexpected value fall back to the shared 8×8 matrix.
        _ => bayer8(x, y),
    }
}

/// Snap `p` to its nearest entry in `ramp` (ordered dark→light) by luma, then
/// step `delta` entries along it (clamped to the ends). Alpha is preserved.
pub fn shade_ramp(p: [u8; 4], ramp: &[[u8; 4]], delta: i32) -> [u8; 4] {
    let lp = luma(p) as i32;
    let nearest = ramp
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| (luma(**c) as i32 - lp).abs())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let i = (nearest as i32 + delta).clamp(0, ramp.len() as i32 - 1) as usize;
    let c = ramp[i];
    [c[0], c[1], c[2], p[3]]
}

/// Ramp-free HSL shade: `dir` +1 lights (+12%/step lightness, hue warms toward
/// 50°), −1 shadows (−12%/step, hue cools toward 250°). Alpha is preserved.
pub fn shade_hsl(p: [u8; 4], dir: i32, steps: i32) -> [u8; 4] {
    let (h, s, l) = rgb_to_hsl(p[0], p[1], p[2]);
    let amt = 0.12 * steps as f32;
    let target = if dir > 0 { 50.0 } else { 250.0 };
    // Shortest-arc nudge of the hue toward the warm/cool target.
    let mut diff = (target - h).rem_euclid(360.0);
    if diff > 180.0 {
        diff -= 360.0;
    }
    let hue = h + diff * (0.2 * steps as f32).min(1.0);
    let nl = (l + dir as f32 * amt).clamp(0.0, 1.0);
    let rgb = hsl_to_rgb(hue, s, nl);
    [rgb[0], rgb[1], rgb[2], p[3]]
}

/// Sample the colour at parameter `t` (0..1) across sorted `stops`. `dither`
/// "bayer"/"noise" picks one of the two bracketing stop colours by an ordered
/// threshold (the classic pixel-art look, palette-true); anything else lerps.
pub fn sample_gradient(
    stops: &[(f32, [u8; 4])],
    t: f32,
    dither: &str,
    x: i32,
    y: i32,
    seed: u64,
) -> [u8; 4] {
    if stops.len() == 1 || t <= stops[0].0 {
        return stops[0].1;
    }
    let last = stops.len() - 1;
    if t >= stops[last].0 {
        return stops[last].1;
    }
    let mut i = 0;
    while i + 1 < stops.len() && t > stops[i + 1].0 {
        i += 1;
    }
    let (pa, ca) = stops[i];
    let (pb, cb) = stops[i + 1];
    let f = if pb > pa { (t - pa) / (pb - pa) } else { 0.0 };
    match dither {
        "bayer" => {
            if f > bayer8(x, y) {
                cb
            } else {
                ca
            }
        }
        "noise" => {
            if f > hash2(x, y, seed) as f32 / u32::MAX as f32 {
                cb
            } else {
                ca
            }
        }
        _ => {
            let l = |a: u8, b: u8| {
                (a as f32 + (b as f32 - a as f32) * f)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            [
                l(ca[0], cb[0]),
                l(ca[1], cb[1]),
                l(ca[2], cb[2]),
                l(ca[3], cb[3]),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
