//! Colour math: HSL and OKLab/OKLCh conversions, WCAG contrast, shading
//! ramps and palette machinery (nearest-colour, median-cut quantisation).

/// Manhattan colour distance over all 4 channels within tolerance.
pub fn close(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    let d: i32 = (0..4).map(|i| (a[i] as i32 - b[i] as i32).abs()).sum();
    d <= tol
}

/// Colour match by MAX channel distance over RGB only (alpha ignored) — the
/// metric the fill/replace tools actually promise ("max channel distance"), and
/// the one that lets an anti-aliased edge (same RGB, different alpha) still
/// match instead of being left as a halo of the old colour.
pub fn close_rgb(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    (0..3).all(|i| (a[i] as i32 - b[i] as i32).abs() <= tol)
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

// -- OKLab / OKLCh perceptual colour space ----------------------------------
//
// Björn Ottosson's OKLab: a perceptually uniform space where equal numeric
// steps in L look like equal steps in brightness, and Euclidean distance
// approximates perceived colour difference. atelier's ramps, quantize and
// palette-snap all live in sRGB+HSL today, which crushes the midtones and
// picks perceptually-wrong nearest colours; OKLab fixes both.

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB (0..255) → OKLab `(L, a, b)`. L is perceptual lightness in [0,1]; a/b
/// are the green–red and blue–yellow opponent axes (roughly ±0.4).
// The matrix constants are OKLab's canonical f64 values; keep them verbatim.
#[allow(clippy::excessive_precision)]
pub fn srgb_to_oklab(c: [u8; 4]) -> (f32, f32, f32) {
    let r = srgb_to_linear(c[0] as f32 / 255.0);
    let g = srgb_to_linear(c[1] as f32 / 255.0);
    let b = srgb_to_linear(c[2] as f32 / 255.0);
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_55 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    (
        0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    )
}

/// OKLab `(L, a, b)` → linear RGB (unclamped) — the one copy of the OKLab
/// matrix, shared by the sRGB encoder and the gamut check.
#[allow(clippy::excessive_precision)]
fn oklab_to_linear_rgb(lab: (f32, f32, f32)) -> (f32, f32, f32) {
    let (l, a, b) = lab;
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    (
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    )
}

/// OKLab `(L, a, b)` → sRGB (0..255), gamut-clamped. Alpha is the caller's job.
pub fn oklab_to_srgb(lab: (f32, f32, f32)) -> [u8; 3] {
    let (r, g, bl) = oklab_to_linear_rgb(lab);
    [
        (linear_to_srgb(r) * 255.0).round().clamp(0.0, 255.0) as u8,
        (linear_to_srgb(g) * 255.0).round().clamp(0.0, 255.0) as u8,
        (linear_to_srgb(bl) * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// Whether an OKLCh colour is inside the sRGB gamut (its linear RGB all in
/// [0,1]) — so a ramp can reduce chroma to fit instead of letting the per-channel
/// clamp in `oklab_to_srgb` shift its hue.
fn oklch_in_gamut(l: f32, c: f32, h: f32) -> bool {
    let (r, g, bl) = oklab_to_linear_rgb(oklch_to_oklab((l, c, h)));
    let eps = 0.001;
    [r, g, bl].iter().all(|&v| v >= -eps && v <= 1.0 + eps)
}

/// OKLCh → sRGB with chroma binary-searched down until the colour is in gamut —
/// a vivid step desaturates EVENLY (L and hue preserved) instead of being
/// per-channel clamped, which shifts the hue (e.g. a bright red → orange).
fn oklch_to_srgb_gamut(l: f32, c: f32, h: f32) -> [u8; 3] {
    if oklch_in_gamut(l, c, h) {
        return oklab_to_srgb(oklch_to_oklab((l, c, h)));
    }
    let (mut lo, mut hi) = (0.0f32, c);
    for _ in 0..20 {
        let mid = (lo + hi) * 0.5;
        if oklch_in_gamut(l, mid, h) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    oklab_to_srgb(oklch_to_oklab((l, lo, h)))
}

/// OKLab → OKLCh `(L, C, h°)`: chroma magnitude + hue angle in degrees.
pub fn oklab_to_oklch(lab: (f32, f32, f32)) -> (f32, f32, f32) {
    let (l, a, b) = lab;
    let c = (a * a + b * b).sqrt();
    let h = b.atan2(a).to_degrees().rem_euclid(360.0);
    (l, c, h)
}

/// OKLCh `(L, C, h°)` → OKLab `(L, a, b)`.
pub fn oklch_to_oklab(lch: (f32, f32, f32)) -> (f32, f32, f32) {
    let (l, c, h) = lch;
    let r = h.to_radians();
    (l, c * r.cos(), c * r.sin())
}

/// Perceptual colour difference (OKLab ΔE, Euclidean). ~0.02 is a just-
/// noticeable step; > 0.1 reads as a distinct colour. RGB-only (ignores alpha).
pub fn oklab_delta(a: [u8; 4], b: [u8; 4]) -> f32 {
    let (l1, a1, b1) = srgb_to_oklab(a);
    let (l2, a2, b2) = srgb_to_oklab(b);
    ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
}

/// Index of the perceptually nearest entry in `palette` to `p` (OKLab ΔE).
/// Returns None for an empty palette. Converts the probe ONCE and single-passes
/// the palette (the old min_by evaluated both deltas — and re-converted the
/// probe — per comparison). For per-pixel loops prefer [`PaletteLab`].
pub fn nearest_oklab(p: [u8; 4], palette: &[[u8; 4]]) -> Option<usize> {
    if palette.is_empty() {
        return None;
    }
    Some(nearest_lab(
        srgb_to_oklab(p),
        palette.iter().map(|c| srgb_to_oklab(*c)),
    ))
}

/// Index of the nearest lab entry to `probe` by squared ΔE — the one copy of
/// the nearest-colour scan shared by `nearest_oklab` and `PaletteLab`.
fn nearest_lab<I: Iterator<Item = (f32, f32, f32)>>(probe: (f32, f32, f32), labs: I) -> usize {
    let (l, a, b) = probe;
    let (mut best, mut bd) = (0usize, f32::MAX);
    for (i, (l2, a2, b2)) in labs.enumerate() {
        let d = (l - l2).powi(2) + (a - a2).powi(2) + (b - b2).powi(2);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best
}

/// Precomputed OKLab palette for per-pixel nearest-colour loops: the palette
/// converts once, and lookups memoize per distinct probe RGB — pixel art has
/// dozens of distinct colours but thousands of pixels, so the powf-heavy
/// sRGB→OKLab conversion drops from per-pixel to per-distinct-colour.
pub struct PaletteLab {
    palette: Vec<[u8; 4]>,
    labs: Vec<(f32, f32, f32)>,
    memo: std::collections::HashMap<[u8; 3], usize>,
}

impl PaletteLab {
    pub fn new(palette: &[[u8; 4]]) -> Self {
        PaletteLab {
            palette: palette.to_vec(),
            labs: palette.iter().map(|c| srgb_to_oklab(*c)).collect(),
            memo: std::collections::HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.palette.is_empty()
    }

    pub fn color(&self, i: usize) -> [u8; 4] {
        self.palette[i]
    }

    /// Index of the perceptually nearest palette entry (alpha ignored), or
    /// None for an empty palette.
    pub fn nearest(&mut self, p: [u8; 4]) -> Option<usize> {
        if self.palette.is_empty() {
            return None;
        }
        let key = [p[0], p[1], p[2]];
        if let Some(&i) = self.memo.get(&key) {
            return Some(i);
        }
        let best = nearest_lab(srgb_to_oklab(p), self.labs.iter().copied());
        self.memo.insert(key, best);
        Some(best)
    }
}

/// A perceptually-even shading ramp built in OKLCh, darkest → lightest.
///
/// Unlike [`make_ramp`] (linear HSL, which bunches the midtones), every step is
/// an equal stride in perceptual lightness between `value_lo`..`value_hi`
/// (OKLab L, 0..1). `hue_shift` is the total hue rotation across the ramp
/// (lighter end warm-shifted, darker end cool-shifted — the classic move).
/// `sat_curve` shapes chroma: `"flat"` holds the base chroma, `"arc"` peaks it
/// at the midtone (the painterly default), `"sat-in-shadow"` pushes chroma into
/// the darks. `anchor_midtone` forces the centre step to be exactly `base`.
pub fn make_ramp_oklch(
    base: [u8; 4],
    count: usize,
    value_lo: f32,
    value_hi: f32,
    hue_shift: f32,
    sat_curve: &str,
    anchor_midtone: bool,
) -> Vec<[u8; 4]> {
    let count = count.max(1);
    let (_lb, cb, hb) = oklab_to_oklch(srgb_to_oklab(base));
    let mid = (count - 1) / 2;
    (0..count)
        .map(|i| {
            if anchor_midtone && i == mid && count > 1 {
                return base;
            }
            let t = if count == 1 {
                0.5
            } else {
                i as f32 / (count - 1) as f32
            };
            let l = value_lo + (value_hi - value_lo) * t;
            let h = hb + (t - 0.5) * hue_shift;
            let c = match sat_curve {
                "flat" => cb,
                // peak chroma at the midtone, falling toward both ends
                "arc" => cb * (1.0 - 0.55 * (2.0 * t - 1.0).powi(2)),
                // richer colour in the shadows, desaturating into the light
                "sat-in-shadow" => cb * (1.15 - 0.5 * t),
                _ => cb,
            }
            .max(0.0);
            // Gamut-map: a vivid step reduces chroma to fit sRGB (even
            // desaturation) rather than clamping channels (which shifts hue).
            let rgb = oklch_to_srgb_gamut(l, c, h);
            [rgb[0], rgb[1], rgb[2], base[3]]
        })
        .collect()
}

/// Median-cut colour quantisation: reduce `pixels` (opaque RGB) to at most `n`
/// representative colours by recursively splitting the colour box along its
/// longest axis at the median, then averaging each box.
pub fn median_cut(pixels: &[[u8; 3]], n: usize) -> Vec<[u8; 4]> {
    if pixels.is_empty() {
        return vec![[0, 0, 0, 255]];
    }
    // Weight-1 pairs make the weighted median the plain median — one
    // implementation of the split loop instead of two.
    let pairs: Vec<([u8; 3], u64)> = pixels.iter().map(|p| (*p, 1u64)).collect();
    median_cut_weighted(&pairs, n.max(1), &[])
}

/// Frequency-weighted median-cut quantisation over `(colour, count)` pairs:
/// boxes split at the WEIGHTED median (a colour used 5000× pulls the cut toward
/// itself; a 3-pixel accent no longer wins a box by mere variety) and each box
/// averages weighted. Pass deduped pixels with their counts. `pinned` colours
/// are always included and consume their share of `n`.
pub fn median_cut_weighted(
    pixels: &[([u8; 3], u64)],
    n: usize,
    pinned: &[[u8; 4]],
) -> Vec<[u8; 4]> {
    let mut out: Vec<[u8; 4]> = pinned.to_vec();
    let want = n.max(1).saturating_sub(out.len());
    if pixels.is_empty() || want == 0 {
        // Pins are "must keep": when they already fill the budget, the
        // palette is exactly the pins — never one bonus derived colour.
        if out.is_empty() {
            out.push([0, 0, 0, 255]);
        }
        return out;
    }
    let mut boxes: Vec<Vec<([u8; 3], u64)>> = vec![pixels.to_vec()];
    while boxes.len() < want {
        // Pick the splittable box with the widest channel range.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .max_by_key(|(_, b)| {
                (0..3)
                    .map(|c| {
                        let (mn, mx) = b.iter().fold((255u8, 0u8), |(mn, mx), (p, _)| {
                            (mn.min(p[c]), mx.max(p[c]))
                        });
                        mx - mn
                    })
                    .max()
                    .unwrap_or(0)
            });
        let Some((bi, _)) = pick else { break };
        let axis = (0..3)
            .max_by_key(|&c| {
                let (mn, mx) = boxes[bi].iter().fold((255u8, 0u8), |(mn, mx), (p, _)| {
                    (mn.min(p[c]), mx.max(p[c]))
                });
                mx - mn
            })
            .unwrap();
        boxes[bi].sort_by_key(|(p, _)| p[axis]);
        // Split at the weighted median so heavily-used colours dominate the cut.
        let total: u64 = boxes[bi].iter().map(|(_, w)| w).sum();
        let mut acc = 0u64;
        let mut mid = boxes[bi].len() / 2;
        for (i, (_, w)) in boxes[bi].iter().enumerate() {
            acc += w;
            if acc * 2 >= total {
                mid = (i + 1).min(boxes[bi].len() - 1).max(1);
                break;
            }
        }
        let hi = boxes[bi].split_off(mid);
        if hi.is_empty() {
            break;
        }
        boxes.push(hi);
    }
    out.extend(boxes.iter().map(|b| {
        let (mut r, mut g, mut bl, mut wsum) = (0u64, 0u64, 0u64, 0u64);
        for (p, w) in b {
            r += p[0] as u64 * w;
            g += p[1] as u64 * w;
            bl += p[2] as u64 * w;
            wsum += w;
        }
        let wsum = wsum.max(1);
        [(r / wsum) as u8, (g / wsum) as u8, (bl / wsum) as u8, 255]
    }));
    out
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
    /// Lightness moved per step.
    const LIGHT_STEP: f32 = 0.12;
    /// Hue targets: highlights warm toward orange, shadows cool toward blue.
    const WARM_HUE: f32 = 50.0;
    const COOL_HUE: f32 = 250.0;
    /// Fraction of the hue gap closed per step.
    const HUE_PULL: f32 = 0.2;
    let (h, s, l) = rgb_to_hsl(p[0], p[1], p[2]);
    let amt = LIGHT_STEP * steps as f32;
    let target = if dir > 0 { WARM_HUE } else { COOL_HUE };
    // Shortest-arc nudge of the hue toward the warm/cool target.
    let mut diff = (target - h).rem_euclid(360.0);
    if diff > 180.0 {
        diff -= 360.0;
    }
    let hue = h + diff * (HUE_PULL * steps as f32).min(1.0);
    let nl = (l + dir as f32 * amt).clamp(0.0, 1.0);
    let rgb = hsl_to_rgb(hue, s, nl);
    [rgb[0], rgb[1], rgb[2], p[3]]
}
