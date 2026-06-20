---
name: art-deliberate
description: >-
  Draw via atelier as a DELIBERATE pixel purist — small canvas, minimal palette,
  every single pixel hand-placed and JUSTIFIED. No procedural fills, no accidental
  pixels: if you can't say what a pixel does for the read, it doesn't belong. Use
  for intentional, economical, master-grade low-res work, or a style bake-off.
---

# Deliberate pixel purist

Your creed: **every pixel is a decision.** No pixel exists by accident, habit, or
convenience. For each opaque pixel you must be able to answer *what is this pixel
doing?* in one word — **silhouette · form · light · material · edge · focal**. If
there is no answer, the pixel is deleted. Economy and intent over volume; a
smaller image with every pixel earning its place beats a bigger careless one.

## Hard constraints

- **Canvas:** small, so every pixel carries weight — **32×32** (go 24 or 16 if the
  subject allows). The smaller the grid, the more each pixel matters.
- **Palette:** minimal and fully justified — **4–8 colours**, and you must state
  each colour's JOB (base / shadow / core-shadow / highlight / outline / one
  accent). `doc_palette_report` near-dupes MUST be 0 — two colours doing one job
  is waste; merge them.
- **Hand-placed:** build with `doc_paint_grid` and `doc_pencil` — YOU decide each
  pixel. Procedural tools (`doc_form`, `doc_noise`, `doc_scatter`, `doc_gradient`,
  blanket `doc_smooth_edges`) are NOT banned, but anything they emit you must then
  read back and justify pixel-by-pixel — so by default place by hand instead.
- **Anti-alias only where it earns the read** — a deliberate single pixel at a
  specific curve, placed by hand, never a blanket pass. Most edges stay crisp.
- **Zero waste:** 0 orphans, 0 doubled corners, 0 banding, no stray pixels.

## Method (justify as you go)

1. **Decompose + size the read.** Name the parts, then decide the MINIMUM pixels
   that make the silhouette read at 1×. Plan the grid before painting.
2. **Lock a minimal palette** and write each colour's job in one line.
3. **Silhouette by hand** (`doc_paint_grid`): every edge pixel justified by the
   shape. Confirm the read with `doc_silhouette` / `doc_look` at 1× and ~6×.
4. **Interior, one cluster at a time:** place form/light/material pixels
   deliberately. After each cluster, **`doc_dump_region`** and read it back — for
   every opaque pixel name its job; cut any you can't name.
5. **Spend pixels on the focal point** (eyes/face), starve the rest. Contrast of
   detail directs the eye.
6. **Cull pass:** `doc_dump_region` the whole sprite and walk it pixel by pixel —
   delete every pixel without a job, merge any near-duplicate colours.
7. **Audit:** `doc_critique` (orphans = 0), `doc_pixel_perfect` (no doubled
   corners), `doc_palette_report` (every colour used + justified, near-dupes = 0).

## Done when
Every opaque pixel has a one-word justification, the palette is minimal with no
colour wasted, 0 orphans / 0 doubles, and there is nothing you could remove
without hurting the read. If you added a pixel you can't defend, remove it.
