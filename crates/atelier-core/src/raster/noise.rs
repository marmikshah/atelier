//! Procedural noise (fBm/Perlin/Voronoi), the seedable pixel hash, ordered
//! dither thresholds, gradient sampling and easing curves.

/// Linear interpolation between `a` and `b` by `t`.
fn lerpf(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Easing of a 0..1 progress `t`. "ease-in" t³ (slow start), "ease-out" the
/// mirror (slow end), "ease-in-out" the symmetric blend, "bounce" (ease-out
/// bounce), "overshoot" (back ease-out — shoots past 1 then settles), "elastic"
/// (decaying oscillation ease-out); anything else linear. Every curve satisfies
/// f(0)=0 and f(1)=1 exactly. Used by keyframe motion so a tween shapes its
/// acceleration. The non-monotone curves (overshoot/elastic) can exceed [0,1].
/// The recognised easing names, hyphenated (`ease` also accepts underscore
/// spellings). The guard callers run so a typo errors instead of silently
/// falling back to linear — the mistake class `valid_blend` exists to catch.
pub const EASE_NAMES: [&str; 7] = [
    "linear",
    "ease-in",
    "ease-out",
    "ease-in-out",
    "bounce",
    "overshoot",
    "elastic",
];

/// Error unless `kind` names a real easing (either separator spelling).
pub fn validate_ease(kind: &str) -> Result<(), String> {
    if EASE_NAMES.contains(&kind.replace('_', "-").as_str()) {
        Ok(())
    } else {
        Err(format!(
            "unknown easing '{kind}' — use one of [{}]",
            EASE_NAMES.join(", ")
        ))
    }
}

pub fn ease(t: f32, kind: &str) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match kind.replace('_', "-").as_str() {
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
        "bounce" => bounce_out(t),
        "overshoot" => {
            // Back ease-out: a single overshoot past 1 that settles back to 1.
            const C1: f32 = 1.70158;
            const C3: f32 = C1 + 1.0;
            let u = t - 1.0;
            1.0 + C3 * u * u * u + C1 * u * u
        }
        "elastic" => {
            // Decaying sine oscillation that converges on 1 (ease-out elastic).
            if t == 0.0 || t == 1.0 {
                t
            } else {
                const P: f32 = std::f32::consts::TAU / 3.0;
                2f32.powf(-10.0 * t) * ((t * 10.0 - 0.75) * P).sin() + 1.0
            }
        }
        // "linear"; unknowns are screened by `validate_ease` at the API edge,
        // and an unscreened caller still degrades to linear rather than panic.
        _ => t,
    }
}

/// Ease-out bounce: `t` decelerates in a series of shrinking bounces, staying
/// within [0,1] with f(0)=0 and f(1)=1. The classic 4-segment piecewise curve.
fn bounce_out(t: f32) -> f32 {
    const N1: f32 = 7.5625;
    const D1: f32 = 2.75;
    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        let t = t - 1.5 / D1;
        N1 * t * t + 0.75
    } else if t < 2.5 / D1 {
        let t = t - 2.25 / D1;
        N1 * t * t + 0.9375
    } else {
        let t = t - 2.625 / D1;
        N1 * t * t + 0.984375
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

/// Interleaved-gradient-noise threshold in [0,1) at (x,y) — Jorge Jimenez's
/// cheap blue-noise-ish dither, less regular than Bayer (no visible matrix
/// grid), used by `doc_dither_ramp`'s `ign` pattern.
pub fn ign(x: i32, y: i32) -> f32 {
    let v = 52.982_918 * (0.067_110_56 * x as f32 + 0.005_837_15 * y as f32).fract();
    v.fract().abs()
}

/// Ordered/blue-noise threshold in [0,1) at (x,y) for a graduated-ramp dither.
/// Adds `ign` on top of the patterns `dither_threshold` knows.
pub fn ramp_dither_threshold(pattern: &str, x: i32, y: i32) -> f32 {
    if pattern == "ign" {
        ign(x, y)
    } else {
        dither_threshold(pattern, x, y)
    }
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
