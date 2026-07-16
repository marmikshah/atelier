//! Geometric transforms and scalar fields: area downscale, interior distance
//! fields and background removal.

use image::{Rgba, RgbaImage};

use super::colour::oklab_delta;

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
