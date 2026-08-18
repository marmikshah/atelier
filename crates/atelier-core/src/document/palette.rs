//! Palette ops: the swatch list, recolour swaps and on-palette snapping.

use image::Rgba;

use crate::raster;

use super::{AlphaSnap, Document};

impl Document {
    // -- palette (indexed-friendly swatch list) -----------------------------

    /// Replace the document's palette swatch list.
    pub fn set_palette(&mut self, colors: Vec<[u8; 4]>) {
        self.meta.palette = colors;
    }

    /// Swap a set of colours across the whole document in one pass — the
    /// recolour-variant workflow (one sprite, many palettes). For every cel
    /// (filtered by optional `layer`/`frame`), every pixel matching a `from`
    /// exactly (all 4 channels) becomes its `to`; stored palette entries that
    /// match a `from` are updated too. Returns the count of pixels changed.
    pub fn palette_swap(
        &mut self,
        pairs: &[([u8; 4], [u8; 4])],
        layer: Option<usize>,
        frame: Option<usize>,
    ) -> Result<u32, String> {
        self.check_palette_scope(layer, frame)?;
        let mut changed = 0;
        for ((l, f), (_x, _y, img)) in self.cels.iter_mut() {
            if layer.is_some_and(|sel| sel != *l) || frame.is_some_and(|sel| sel != *f) {
                continue;
            }
            let before = changed;
            for p in img.pixels_mut() {
                if let Some((_, to)) = pairs.iter().find(|(from, _)| p.0 == *from) {
                    *p = Rgba(*to);
                    changed += 1;
                }
            }
            if changed > before {
                // This loop mutates cels in place (no cel_canvas), so the
                // dirty marking has to happen here, per touched cel.
                self.dirty.insert((*l, *f));
            }
        }
        // Keep the stored palette consistent with the recolour.
        for c in self.meta.palette.iter_mut() {
            if let Some((_, to)) = pairs.iter().find(|(from, _)| *c == *from) {
                *c = *to;
            }
        }
        Ok(changed)
    }

    /// Snap one cel back to the document's own locked palette — the
    /// post-generator discipline pass every craft tool runs. No palette = no-op.
    pub fn snap_cel_to_own_palette(
        &mut self,
        layer: usize,
        frame: usize,
        alpha: AlphaSnap,
    ) -> Result<u32, String> {
        let pal = self.meta.palette.clone();
        self.snap_to_palette(&pal, Some(layer), Some(frame), alpha)
    }

    /// Snap every opaque pixel to its perceptually nearest palette swatch
    /// (OKLab ΔE), preserving alpha. Scope to a `layer`/`frame` or the whole
    /// document. Kills the off-palette drift that blends/dithers/effects leave
    /// behind. Returns the number of pixels moved to a different colour.
    pub fn snap_to_palette(
        &mut self,
        palette: &[[u8; 4]],
        layer: Option<usize>,
        frame: Option<usize>,
        alpha: AlphaSnap,
    ) -> Result<u32, String> {
        self.check_palette_scope(layer, frame)?;
        if palette.is_empty() {
            return Ok(0);
        }
        let mut changed = 0;
        let mut lab = raster::PaletteLab::new(palette);
        for ((l, f), (_x, _y, img)) in self.cels.iter_mut() {
            if layer.is_some_and(|s| s != *l) || frame.is_some_and(|s| s != *f) {
                continue;
            }
            let before = changed;
            for p in img.pixels_mut() {
                let src = p.0;
                if src[3] == 0 {
                    continue;
                }
                // Decide the colour to snap and the alpha to keep, per policy.
                // `Opaque` collapses a continuous-tone FX bloom into a crisp
                // on-palette silhouette; `Flatten` melts it onto a known backdrop.
                let (rgb_in, out_alpha, clear) = match alpha {
                    AlphaSnap::Preserve => (src, src[3], false),
                    AlphaSnap::Opaque(cut) => (src, 255u8, src[3] < cut),
                    AlphaSnap::Flatten(bg) => (raster::over(bg, src), 255u8, false),
                };
                let new = if clear {
                    [0, 0, 0, 0]
                } else if let Some(i) = lab.nearest(rgb_in) {
                    let c = lab.color(i);
                    [c[0], c[1], c[2], out_alpha]
                } else {
                    src
                };
                if new != src {
                    *p = Rgba(new);
                    changed += 1;
                }
            }
            if changed > before {
                // In-place mutation (no cel_canvas) — mark per touched cel.
                self.dirty.insert((*l, *f));
            }
        }
        Ok(changed)
    }

    fn check_palette_scope(
        &self,
        layer: Option<usize>,
        frame: Option<usize>,
    ) -> Result<(), String> {
        if let Some(layer) = layer
            && layer >= self.meta.layers.len()
        {
            return Err(format!(
                "no layer {layer} (layers={})",
                self.meta.layers.len()
            ));
        }
        if let Some(frame) = frame
            && frame >= self.meta.frames.len()
        {
            return Err(format!(
                "no frame {frame} (frames={})",
                self.meta.frames.len()
            ));
        }
        Ok(())
    }
}
