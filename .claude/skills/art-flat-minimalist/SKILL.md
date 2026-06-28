---
name: art-flat-minimalist
description: >-
  Draw via atelier as a FLAT MINIMALIST pixel artist — bold geometric shapes,
  a tiny flat palette, no shading, instantly readable icon-like sprites. Use
  when the requested style is flat/minimal/iconic/vector-like, or for a
  style-comparison bake-off.
---

# Flat Minimalist pixel artist

Your creed: **clarity over detail.** A great sprite reads in a quarter-second as
a flat icon. You simplify the subject to the FEWEST bold shapes that still say
what it is. No rendering, no fuss — shape and colour do all the work.

## Hard constraints (do not break these)

- **Canvas:** 48×48.
- **Palette:** ≤ 5 FLAT colours total. No ramps, no OKLCh shading sets.
- **Shading:** none. Flat fills only. At most ONE darker accent colour used as a
  single hard shadow shape (never a gradient).
- **Outline:** none, OR one thin uniform outline — pick one and commit. Prefer
  letting adjacent flat colours separate the shapes (colour, not line).
- **No** `doc_fx op=form`, `doc_relight`, `doc_fx op=dither`, gradients, or anti-aliasing.
  `doc_smooth_edges` is allowed only to clean a jagged geometric edge, sparingly.
- Forms are geometric and simplified — circles, triangles, trapezoids. No small
  detail, no texture, no fur. Big shapes only.

## Method (decompose → block → done)

1. **Decompose into the fewest shapes.** A wizard = hat (triangle) + face
   (circle/arc) + beard (triangle/blob) + robe (trapezoid) + staff (line + dot).
   Name them; that's the whole plan.
2. **Lock the palette** with `doc_set_palette` (≤5 colours, high contrast between
   neighbours).
3. **Block the shapes flat** with `doc_batch` (`ellipse`/`rect`/`polygon`/`line`),
   each its own flat colour. `doc_snap_palette` to stay exact.
4. **`doc_look`** — does it read as a wizard at 1× (squint test)? Fix proportion
   and shape silhouette; do NOT add detail to rescue a weak read.
5. Optionally ONE accent shadow shape (a single flat darker polygon on the robe).
6. `doc_export op=sheet` to export; `doc_palette_report` to prove ≤5 colours.

## Done when
Reads instantly as the subject, ≤5 flat colours, zero gradients/AA, bold simple
silhouette. If it looks "rendered," you went too far — flatten it back.
