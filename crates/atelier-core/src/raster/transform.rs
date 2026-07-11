//! Geometric transforms and scalar fields: RotSprite rotate/affine, area /
//! majority downscales, two-bone IK, distance fields and background removal.

use image::{Rgba, RgbaImage};

use super::colour::oklab_delta;

/// Rotate `src` by `deg` (clockwise) about its centre with nearest-neighbour
/// sampling, returning a new image sized to the rotated bounding box.
pub fn rotate_nn(src: &RgbaImage, deg: f32) -> RgbaImage {
    // Pure rotation is the no-scale, no-shear case of the affine RotSprite
    // pipeline; routing through it (supersample 2×) keeps clusters from
    // shattering and never mints off-palette fringe, vs the old raw NN rotate.
    affine_nn(src, deg, 1.0, 1.0, 0.0, 0.0, 2)
}

/// EPX / Scale2x edge-preserving 2× upscale: each pixel becomes 2×2, and a
/// sub-pixel only takes a neighbour's colour when two orthogonal neighbours
/// agree and the diagonal disagrees (smooths a staircase without inventing
/// colours). Emits ONLY colours already in `src` — the property that lets the
/// RotSprite pipeline rotate without minting off-palette fringe.
fn scale2x(src: &RgbaImage) -> RgbaImage {
    let (w, h) = (src.width(), src.height());
    let mut out = RgbaImage::new(w * 2, h * 2);
    let get = |x: i32, y: i32| -> [u8; 4] {
        let cx = x.clamp(0, w as i32 - 1) as u32;
        let cy = y.clamp(0, h as i32 - 1) as u32;
        src.get_pixel(cx, cy).0
    };
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let p = get(x, y);
            let (a, b, c, d) = (get(x, y - 1), get(x + 1, y), get(x - 1, y), get(x, y + 1));
            let e0 = if c == a && c != d && a != b { a } else { p };
            let e1 = if a == b && a != c && b != d { b } else { p };
            let e2 = if d == c && d != b && c != a { c } else { p };
            let e3 = if b == d && b != a && d != c { d } else { p };
            let (ox, oy) = (x as u32 * 2, y as u32 * 2);
            out.put_pixel(ox, oy, Rgba(e0));
            out.put_pixel(ox + 1, oy, Rgba(e1));
            out.put_pixel(ox, oy + 1, Rgba(e2));
            out.put_pixel(ox + 1, oy + 1, Rgba(e3));
        }
    }
    out
}

/// Downscale by an integer `factor` by majority vote: each output pixel is the
/// most common colour in its `factor×factor` source block (ties broken toward
/// the more opaque colour, then deterministically by channel). Unlike a bilinear
/// downscale this emits ONLY colours present in the source — no blended fringe,
/// so a palette stays intact through a rotate/scale.
fn majority_downscale(src: &RgbaImage, factor: u32) -> RgbaImage {
    if factor <= 1 {
        return src.clone();
    }
    let (w, h) = (
        (src.width() / factor).max(1),
        (src.height() / factor).max(1),
    );
    let mut out = RgbaImage::new(w, h);
    // A block holds ≤ factor² distinct colours (≤16 on the RotSprite path) —
    // a stack array beats allocating a HashMap per output pixel.
    let mut counts: Vec<([u8; 4], u32)> = Vec::with_capacity((factor * factor) as usize);
    for oy in 0..h {
        for ox in 0..w {
            counts.clear();
            for dy in 0..factor {
                for dx in 0..factor {
                    let (sx, sy) = (ox * factor + dx, oy * factor + dy);
                    if sx < src.width() && sy < src.height() {
                        let px = src.get_pixel(sx, sy).0;
                        match counts.iter_mut().find(|(c, _)| *c == px) {
                            Some((_, n)) => *n += 1,
                            None => counts.push((px, 1)),
                        }
                    }
                }
            }
            // Total ordering so ties resolve deterministically: count, then
            // alpha (opaque wins so silhouette edges survive), then channels.
            let best = counts
                .iter()
                .copied()
                .max_by_key(|(c, n)| (*n, c[3], c[0], c[1], c[2]))
                .map(|(c, _)| c)
                .unwrap_or([0, 0, 0, 0]);
            out.put_pixel(ox, oy, Rgba(best));
        }
    }
    out
}

/// General affine transform about the image centre — rotate (deg, clockwise),
/// non-uniform scale (`sx`,`sy`) and shear (`skew_x`,`skew_y` in degrees), in
/// that compose order (scale → shear → rotate). Returns a new image sized to
/// the transformed bounding box, sampled by nearest-neighbour. `supersample`
/// (1..=4) renders at N× with an edge-preserving Scale2x upscale, transforms,
/// then majority-vote downscales — a RotSprite pipeline that keeps rotated /
/// scaled pixel clusters from shattering AND never mints off-palette fringe.
pub fn affine_nn(
    src: &RgbaImage,
    rot_deg: f32,
    sx: f32,
    sy: f32,
    skew_x_deg: f32,
    skew_y_deg: f32,
    supersample: u32,
) -> RgbaImage {
    // Round the supersample up to a power of two (Scale2x doubles per pass):
    // 1 -> none, 2 -> ×2, 3/4 -> ×4. `factor` is the true upscale used below.
    let ss = supersample.clamp(1, 4);
    let factor: u32 = if ss <= 1 {
        1
    } else if ss <= 2 {
        2
    } else {
        4
    };
    let mut work = src.clone();
    let mut f = factor;
    while f > 1 {
        work = scale2x(&work);
        f /= 2;
    }
    let (w, h) = (work.width() as f32, work.height() as f32);
    let r = rot_deg.to_radians();
    let (cos, sin) = (r.cos(), r.sin());
    let kx = skew_x_deg.to_radians().tan();
    let ky = skew_y_deg.to_radians().tan();
    // M = R * H * S  (column-vector convention)
    let m00 = cos * sx - sin * ky * sx;
    let m01 = cos * kx * sy - sin * sy;
    let m10 = sin * sx + cos * ky * sx;
    let m11 = sin * kx * sy + cos * sy;
    let det = m00 * m11 - m01 * m10;
    if det.abs() < 1e-6 {
        return RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0]));
    }
    let (i00, i01, i10, i11) = (m11 / det, -m01 / det, -m10 / det, m00 / det);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let fwd = |x: f32, y: f32| (m00 * x + m01 * y, m10 * x + m11 * y);
    let corners = [fwd(-cx, -cy), fwd(cx, -cy), fwd(-cx, cy), fwd(cx, cy)];
    let minx = corners.iter().map(|p| p.0).fold(f32::MAX, f32::min);
    let maxx = corners.iter().map(|p| p.0).fold(f32::MIN, f32::max);
    let miny = corners.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let maxy = corners.iter().map(|p| p.1).fold(f32::MIN, f32::max);
    let (nw, nh) = (
        ((maxx - minx).ceil() as u32).max(1),
        ((maxy - miny).ceil() as u32).max(1),
    );
    // Refuse an absurd transform (e.g. scale_x=50000) before allocating — a
    // giant buffer would abort the process. ~4M px (2048²) is the ceiling.
    if (nw as u64) * (nh as u64) > 4_194_304 {
        return RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0]));
    }
    let mut out = RgbaImage::from_pixel(nw, nh, Rgba([0, 0, 0, 0]));
    for oy in 0..nh {
        for ox in 0..nw {
            // Inverse-map the dest pixel CENTRE (not its corner) back to source,
            // then take the source pixel that contains it — so a 90/180/270°
            // rotation is a clean permutation, not a half-pixel-biased resample.
            let (dx, dy) = (minx + ox as f32 + 0.5, miny + oy as f32 + 0.5);
            let sxp = (i00 * dx + i01 * dy + cx).floor();
            let syp = (i10 * dx + i11 * dy + cy).floor();
            if sxp >= 0.0
                && syp >= 0.0
                && (sxp as u32) < work.width()
                && (syp as u32) < work.height()
            {
                let p = *work.get_pixel(sxp as u32, syp as u32);
                if p.0[3] > 0 {
                    out.put_pixel(ox, oy, p);
                }
            }
        }
    }
    if factor > 1 {
        majority_downscale(&out, factor)
    } else {
        out
    }
}

/// Two-bone analytic inverse kinematics (law of cosines). Given a `root` joint
/// (shoulder/hip), an end-effector `target` (hand/foot), and the two bone
/// lengths `l1` (upper) and `l2` (lower), return the middle joint (elbow/knee)
/// position. `bend` (+1 / -1) selects which side the joint bends. The target is
/// clamped to the reachable annulus `[|l1-l2|, l1+l2]` so an out-of-reach foot
/// just straightens the leg instead of producing NaNs.
pub fn solve_ik2(root: (f32, f32), target: (f32, f32), l1: f32, l2: f32, bend: f32) -> (f32, f32) {
    let (dx, dy) = (target.0 - root.0, target.1 - root.1);
    let dist = (dx * dx + dy * dy).sqrt().max(1e-4);
    let lo = (l1 - l2).abs() + 1e-3;
    let hi = (l1 + l2) - 1e-3;
    let d = dist.clamp(lo, hi);
    let base = dy.atan2(dx);
    // interior angle at the root between root->target and the upper bone
    let cos_t = ((l1 * l1 + d * d - l2 * l2) / (2.0 * l1 * d)).clamp(-1.0, 1.0);
    let theta = cos_t.acos();
    let ang = base + bend.signum() * theta;
    (root.0 + l1 * ang.cos(), root.1 + l1 * ang.sin())
}

/// True area-average downscale (box filter over each target pixel's exact
/// source footprint, fractional edges included), alpha-weighted so transparent
/// source pixels don't darken edges. Keeps thin outlines readable where
/// bilinear smears them. Falls back to nearest when not actually shrinking.
pub fn downscale_area(src: &RgbaImage, tw: u32, th: u32) -> RgbaImage {
    let (sw, sh) = (src.width(), src.height());
    let (tw, th) = (tw.max(1), th.max(1));
    if tw >= sw && th >= sh {
        return image::imageops::resize(src, tw, th, image::imageops::FilterType::Nearest);
    }
    let mut out = RgbaImage::new(tw, th);
    let fx = sw as f64 / tw as f64;
    let fy = sh as f64 / th as f64;
    for ty in 0..th {
        let (y0, y1) = (ty as f64 * fy, (ty + 1) as f64 * fy);
        for tx in 0..tw {
            let (x0, x1) = (tx as f64 * fx, (tx + 1) as f64 * fx);
            let (mut r, mut g, mut b, mut a, mut area) = (0f64, 0f64, 0f64, 0f64, 0f64);
            let mut sy = y0.floor() as u32;
            while (sy as f64) < y1 && sy < sh {
                let hy = y1.min(sy as f64 + 1.0) - y0.max(sy as f64);
                let mut sx = x0.floor() as u32;
                while (sx as f64) < x1 && sx < sw {
                    let wx = x1.min(sx as f64 + 1.0) - x0.max(sx as f64);
                    let wgt = wx * hy;
                    let p = src.get_pixel(sx, sy).0;
                    let pa = p[3] as f64 / 255.0;
                    r += p[0] as f64 * pa * wgt;
                    g += p[1] as f64 * pa * wgt;
                    b += p[2] as f64 * pa * wgt;
                    a += pa * wgt;
                    area += wgt;
                    sx += 1;
                }
                sy += 1;
            }
            let px = if a > 1e-9 {
                [
                    (r / a).round().clamp(0.0, 255.0) as u8,
                    (g / a).round().clamp(0.0, 255.0) as u8,
                    (b / a).round().clamp(0.0, 255.0) as u8,
                    (a / area.max(1e-9) * 255.0).round().clamp(0.0, 255.0) as u8,
                ]
            } else {
                [0, 0, 0, 0]
            };
            out.put_pixel(tx, ty, Rgba(px));
        }
    }
    out
}

/// Corner-seeded background removal: flood from each opaque corner over pixels
/// perceptually close to that corner's colour (OKLab ΔE ≤ `tol`), zeroing
/// their alpha. Run BEFORE palette extraction so a backdrop can't steal
/// palette slots from the subject. Corner seeds (not whole-border seeds) on
/// purpose: a subject that touches a canvas edge — feet at the bottom, a
/// full-bleed portrait — must not seed its own deletion. The cost is that
/// strongly-graded backdrops clear only the corner-toned bands; flat and
/// gently-graded backdrops clear fully. Leaves enclosed interior regions alone.
pub fn remove_background(img: &mut RgbaImage, tol: f32) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    if w == 0 || h == 0 {
        return;
    }
    let mut cleared = vec![false; (w * h) as usize];
    let mut stack: Vec<(i32, i32, [u8; 4])> = Vec::new();
    for &(cx, cy) in &[(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
        let seed = img.get_pixel(cx as u32, cy as u32).0;
        if seed[3] > 0 {
            stack.push((cx, cy, seed));
        }
    }
    while let Some((x, y, seed)) = stack.pop() {
        if x < 0 || y < 0 || x >= w || y >= h {
            continue;
        }
        let i = (y * w + x) as usize;
        if cleared[i] {
            continue;
        }
        let p = img.get_pixel(x as u32, y as u32).0;
        if p[3] == 0 || oklab_delta(p, seed) > tol {
            continue;
        }
        cleared[i] = true;
        img.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 0]));
        stack.push((x + 1, y, seed));
        stack.push((x - 1, y, seed));
        stack.push((x, y + 1, seed));
        stack.push((x, y - 1, seed));
    }
}

/// Separable box-blur of a scalar field. Used to smooth the interior-distance
/// height field before it is differentiated into surface normals: the raw
/// chamfer field has a sharp ridge along the medial axis, and central
/// differences across that ridge read as facet creases. A light blur rounds the
/// ridge so a blob lights as a smooth dome. `radius` is per-axis, one pass.
pub fn blur_field(field: &[f32], w: usize, h: usize, radius: i32) -> Vec<f32> {
    if radius <= 0 || w == 0 || h == 0 {
        return field.to_vec();
    }
    let idx = |x: usize, y: usize| y * w + x;
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (mut acc, mut n) = (0.0f32, 0.0f32);
            for k in -radius..=radius {
                let sx = x as i32 + k;
                if sx >= 0 && (sx as usize) < w {
                    acc += field[idx(sx as usize, y)];
                    n += 1.0;
                }
            }
            tmp[idx(x, y)] = acc / n;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (mut acc, mut n) = (0.0f32, 0.0f32);
            for k in -radius..=radius {
                let sy = y as i32 + k;
                if sy >= 0 && (sy as usize) < h {
                    acc += tmp[idx(x, sy as usize)];
                    n += 1.0;
                }
            }
            out[idx(x, y)] = acc / n;
        }
    }
    out
}

/// Normalised interior distance for each cell of a `w`×`h` boolean foreground
/// mask `fg`: 0 on background (and the outermost foreground rim), rising toward
/// 1 at the most-interior foreground pixel. A two-pass 1/√2-weight chamfer
/// distance transform, divided by its max so the field is resolution- and
/// shape-independent. Used by `Document::form` "auto" to give an arbitrary blob
/// volume (bright core, dark edges) without assuming an elliptical outline.
pub fn interior_distance(fg: &[bool], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    const BIG: f32 = 1.0e9;
    const D1: f32 = 1.0;
    const D2: f32 = std::f32::consts::SQRT_2;
    let idx = |x: usize, y: usize| y * w + x;
    let mut d = vec![0.0f32; n];
    for i in 0..n {
        d[i] = if fg[i] { BIG } else { 0.0 };
    }
    // Forward pass (top-left → bottom-right).
    for y in 0..h {
        for x in 0..w {
            if !fg[idx(x, y)] {
                continue;
            }
            let mut m = d[idx(x, y)];
            if x > 0 {
                m = m.min(d[idx(x - 1, y)] + D1);
            }
            if y > 0 {
                m = m.min(d[idx(x, y - 1)] + D1);
            }
            if x > 0 && y > 0 {
                m = m.min(d[idx(x - 1, y - 1)] + D2);
            }
            if x + 1 < w && y > 0 {
                m = m.min(d[idx(x + 1, y - 1)] + D2);
            }
            d[idx(x, y)] = m;
        }
    }
    // Backward pass (bottom-right → top-left).
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            if !fg[idx(x, y)] {
                continue;
            }
            let mut m = d[idx(x, y)];
            if x + 1 < w {
                m = m.min(d[idx(x + 1, y)] + D1);
            }
            if y + 1 < h {
                m = m.min(d[idx(x, y + 1)] + D1);
            }
            if x + 1 < w && y + 1 < h {
                m = m.min(d[idx(x + 1, y + 1)] + D2);
            }
            if x > 0 && y + 1 < h {
                m = m.min(d[idx(x - 1, y + 1)] + D2);
            }
            d[idx(x, y)] = m;
        }
    }
    let mx = d.iter().copied().fold(0.0f32, f32::max).max(1.0);
    for v in d.iter_mut() {
        *v /= mx;
    }
    d
}
