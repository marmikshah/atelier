# atelier — Art-Quality Review & Tool Roadmap

> 30-agent review (5 source readers · 3 live empirical probes that actually drove
> atelier · 17 art-quality dimension auditors · 3 cross-check lenses + 1
> completeness critic · 1 synthesizer). 90 tool proposals collected, deduped, and
> ranked. The mandate: where the toolset falls short of *world-class*, where the
> agent's editing breaks down, and the tools to close the gap.

## 1. Verdict

atelier is a genuinely strong *first-generation* pixel-art engine that has already encoded more craft as API than most editors expose (hue-shifted ramps, interior-distance volume shading, ramp/seam/anim audits, palette-true Bayer). But it sits at roughly the **competent-hobbyist ceiling**, not world-class, and three systemic gaps hold it there. **First, the near-blind agent cannot see the things that separate amateur from master work** — pillow-shading, banding, jaggies, value-massing, dither clumping, light-direction consistency, and silhouette readability all have *zero* detectors, and the primary `doc_render` still returns a file path (only the frame-0 resource returns inline base64), so every "look" is two round-trips and the iteration budget that *is* the quality budget gets halved. **Second, atelier is an all-destructive, paint-only tool with no transform, no rig, no undo, and no structural editing** — you cannot rotate a drawn blade, reorder a layer, isolate a limb, or revert a bad `quantize`, which makes ambitious work physically impossible or too risky to attempt. **Third, everything is intra-document and perceptually shallow** — there is no OKLab anywhere (so ramps are perceptually lumpy and `quantize` picks wrong colours), no cross-document set cohesion (the store has 304 docs in versioned families that nothing can audit as "one game"), and no composite/alpha-aware perception (glass, glow, and translucency are eyeballed). Close those three and atelier reaches portfolio grade.

## 2. The cross-cutting themes

**A. The blind agent's perception debt is the master bottleneck.** atelier's whole premise is an agent that describes art well and sees it badly. Yet the analysis surface measures *occupancy and histograms*, never the named pixel-art failure modes. Banding, pillow-shading (which `form('auto')` actively *causes* via concentric interior-distance falloff), jaggy staircases, dither worms/clumping, value-soup, and inconsistent light direction are all invisible — so the agent applies a craft op and ships its own regressions. Worse, the look itself is slow (path-not-pixels) and alpha-blind. *This caps quality because you cannot iterate toward a target you cannot measure or see cheaply.*

**B. There is no non-destructive / transform / structural backbone.** No undo, no in-place rotate/scale/skew (NN-only, via PNG round-trip), no layer reorder/insert/delete, no region-scoped clear to isolate a limb. The probes hit this wall on *every* run: the swordsman couldn't swing an arm (no rotate) or re-pose one limb (no part isolation); the still-life couldn't stack liquid behind glass (top-append-only layers) or undo a feared `fill_cel`; the dusk scene couldn't insert a haze layer at depth or delete the vestigial base layer. *This caps quality because the experimental, work-in-passes, reposition-and-reuse discipline that defines professional output is structurally forbidden.*

**C. Colour and light are perceptually shallow and single-source.** Everything lives in HSL + gamma-luma. No OKLab means `make_ramp` steps are perceptually uneven (mids crush), `quantize` picks perceptually-wrong nearest colours, and `ramp_validate` certifies lumpy ramps as "even." Lighting is one light, 8 compass directions, fixed z=0.6 — no key/fill/rim, no authored warm-light/cool-shadow, no AO/cast shadow, no placed specular. *This caps quality because perceptually-even ramps under a deliberate multi-light scheme are the literal difference between "shaded" and "painted."*

**D. Optimizes one sprite; the job is a cohesive set.** All 90 proposals and every analysis tool are intra-document. The real workload is dozens of files that must share one palette, one light direction, one scale, one outline convention. There is no project object, no locked cross-doc palette, no set-wide consistency audit. *This caps quality because cohesion across the set — not any single sprite — is the hallmark of shipped pixel art.*

**E. Effect parameters are scalars where craft needs fields.** `scatter` takes one density, `blur` one radius, `gradient`/`noise` fill a whole rect, and only `dither` has `only_existing`. "Grass denser toward camera," "haze only over the pines, not the sky gaps," "shade only what I just drew" are all impossible in one call. *A cheap, general unlock (let existing ops take a falloff field / `only_existing` / a saved-from-previous-op mask) was under-weighted versus piling on new whole-effect tools.*

## 3. Prioritized tool roadmap

Merged per the cross-check audits (transform_cel P42+P52, smear P45+P69, warp P28+P54, the AA family P12/P16/P23, the light/form-audit family P10/P14/P29, notan P11+P33, material P20+P37, dither/texture audit P19+P40, outline P21+P24, import P18+P77, motion-audit P44+P50). Roadmap-dup items kept only where they materially refine the planned line. New structural/plumbing tools from the completeness critic added.

| Tool | Purpose | Quality leap | Builds on | Effort | Tier |
|---|---|---|---|---|---|
| **doc_look** | One-call SEE: inline PNG + value/sat/hue modes + grid/coord ruler + stats | Halves the look round-trip; every look becomes measured | render_preview, value_image, base64 blob (server.rs:1368) | M | **1** |
| **doc_checkpoint** | Snapshot/restore/diff the doc dir; regression deltas (silhouette/colour/contrast) | Turns one-way destructive ops into safe experiments | dir load/save model, frame_diff, palette_report | M | **1** |
| **doc_critique** | Aggregated craft scorecard: banding/pillow/jaggies/orphans/ramp-adherence, optional vs-ref delta | The "art director eye" — makes named failure modes visible | components, render_value bands, ramp_validate, contrast_check | L | **1** |
| **doc_transform_cel** | In-place rotate/scale/skew + RotSprite resample + volume-preserve squash + snap_palette | Clean rotation/scale of drawn art — the #1 missing primitive | copy/paste buffers, rotate_nn, quantize, apply_masked | L | **1** |
| **doc_layer_ops** | Layer lifecycle: move/insert-at/delete/rename/duplicate/merge_down | Unblocks z-order compositing, limb isolation, whole-figure effects | LayerMeta/cels HashMap, flatten | M | **1** |
| **doc_make_perceptual_ramp** | OKLab even-step ramp + chroma arc + midtone anchor + inline validate | Ramps that read as smooth lit form, not banded posterization | make_ramp (→OKLab), ramp_validate | M | **1** |
| **doc_smooth_edges** | Selout: opaque in-ramp AA pixels at staircase corners (outer + interior contours) | Master-grade smooth diagonals vs Bresenham stairs | outline_cel scan, pixel_perfect L-detect, make_ramp | M | **1** |
| **doc_select_render** | Render the active mask (rubylith/ants/coverage) before painting | Ends "paint through an unseen mask with no undo" | Selection.mask, seam/frame_diff overlay path | S | **1** |
| **doc_select_wand** | Contiguous perceptual magic-wand, composite-aware, set-combinable | Precondition for all local recolour/re-shade | bucket_fill flood, doc_select mode logic | S | **1** |
| doc_relight | Multi-light (key/fill/rim, full azimuth/elev, colour, ambient) form shader | "Shaded blob" → "painted form" | form normal-dot math, make_ramp, shade_ramp | L | 2 |
| doc_form_audit | Per-form inferred light vector + pillow-shading + plane-break detection (absorbs light_audit/banding-pillow) | Sees the #1 beginner failure for the first time | components, interior_distance, render_value | M | 2 |
| doc_notan_map | Posterized value masses + adjacent-form merge contrast + notan PNG | Fights gray-soup; value massing the agent can't see today | render_value bands, components | M | 2 |
| doc_dither_ramp | Multi-tone graduated dither over a full ramp; blue-noise/IGN/halftone | Master gradient shading vs 2-colour Bayer | dither_threshold, interior_distance, gradient axis | L | 2 |
| doc_texture_report | Spatial-freq QA: grain, contrast, tile-repeat autocorr, dither uniformity, worms (absorbs dither_audit) | Lets the agent tune texture it can't squint at | analysis reader pattern, render_value, seam autocorr | M | 2 |
| doc_silhouette_audit | Negative-space/protrusion/convex-deficiency + downscaled thumbnail grid | The silhouette/readability test as numbers | doc_silhouette, components, imageops downscale | M | 2 |
| doc_render_value_scoped | Add region+layer params to value/silhouette/coverage/components; subject-vs-bg histogram | Read subject value ignoring the dark background (probe-blocked) | value_image, analysis_image | S | 2 |
| doc_snap_palette | Perceptual nearest-swatch remap of cel/doc to locked palette | Kills the 62-off-palette-colour drift from blend/dither pipelines | quantize (→OKLab), set_palette | S | 2 |
| doc_motion_audit | True-centroid + per-component arc residual, volume constancy, timing series, trajectory PNG | Sees arcs/squash/sub-part motion (anim_audit reads ~0) | play_sequence, components, frame durations | M | 2 |
| doc_contact_sheet | Inline labeled frame grid + adjacent-frame diff tint | The animator's flip-test in one image | render_preview, play_sequence, frame_diff | S | 2 |
| doc_translucency_report | Flattened-composite get_pixel + region mean-alpha + see-through contrast | Makes glass/glow/alpha measurable, not eyeballed | flatten, get_pixel, luma | S | 2 |
| doc_harmony_palette | Multi-ramp harmony (complementary/triadic/…) with shared light/shadow poles, OKLab, wheel PNG | One-ramp-at-a-time → cohesive limited palette | make_perceptual_ramp (×N), set_palette | M | 2 |
| doc_import_clean | AI/photo → pixel: area-downscale + defringe + error-diffusion quantize + cluster-merge + silhouette-snap (absorbs import_diffuse) | The modern reference-onboarding pipeline | stamp_image, median_cut, dither_threshold (→err-diffusion) | L | 2 |
| doc_perspective_guide | Non-destructive VP / 2:1 iso guide layer; returns axis angles | Constructive perspective & iso for props/architecture | line draw, add_layer, clamp_region | M | 3 |
| doc_box | 3-face plane-shaded cuboid (iso/2pt) under one light | Hard-surface building block form() can't make | form light vector, polygon fill, make_ramp | M | 3 |
| doc_material | One-call material recipes (wood/stone/metal/cloth/water/skin/glass) | 6–10 blind calls → "reads as the material" | doc_noise (refined), dither_ramp, make_ramp, specular | L | 3 |
| doc_noise (refine) | Tileable wrap + ridged/billow/turbulence/domain-warp + voronoi F2-F1 + height output | Substrate for natural materials & seamless tiles | fbm/perlin/voronoi (raster.rs) | M | 3 |
| doc_autotile_set + doc_tilemap_assemble | 47-blob/dual-grid bitmask gen **and** mask→autotile→render assembly | Organic terrain + see it assembled (the only real test) | wang_tiles, interior_distance, copy/paste, flatten | L | 3 |
| doc_warp / doc_warp_mesh | Homography quad-projection (iso faces) + lattice squash/liquify (absorbs P28) | 2.5D projection & deformation; refraction-adjacent | rotate_nn inverse-sample, interior_distance, copy_region | L | 3 |
| doc_burst + doc_emit + doc_fx_audit | Radial frame-sequenced FX, seeded particle sim, + flash/expansion/symmetry audit | VFX-as-frames with a verifiable flash frame | gradient/glow, scatter hash2, add_frame, render_value | L | 3 |
| doc_outline_selective | Per-edge light-aware selout + colour-from-fill grading (absorbs outline_color) | Form-following contour vs flat black keyline | outline_cel, shade light_dir, make_ramp | M | 3 |
| doc_font_load + doc_text2 + doc_panel | Bitmap-font registry/BMFont + layout engine + 9-slice panel | Shippable HUD/dialog vs debug font | raster glyph model, over()/composite, copy/paste scale | L | 3 |

*Rigging (define_part/stamp_part/pose_keyframe/ik_pose) is deliberately deferred to §4 — it requires a data-model change, not a tool.*

## 3b. Tier-1 deep dives

**doc_look** — `doc_look(id, frame=0, scale=4, region?, mode='render'|'value-gray'|'value-bands'|'sat'|'hue', grid=false, coords=false, palette_index=false, max_size?, onion=false, return='image'|'image+stats') -> ImageContent + stats?`
The base64 `image/png` blob already works on the resource path (server.rs:1368); this wires it to a *tool result* via `rmcp` Content so the agent sees pixels in the same turn — no separate Read. Folds in `value_image` modes plus a coordinate ruler burned into the upscale margin and an optional palette-index overlay, so one call gives a measured, placeable look. This is the single highest-leverage change in the whole report: it roughly doubles affordable iterations, which compounds across every other tool.

**doc_checkpoint** — `doc_checkpoint(id, action='save'|'list'|'restore'|'diff'|'prune', label?, checkpoint_id?, render_overlay=false) -> {...}`
A document is a self-contained directory (load/save at document.rs:143–178), so a checkpoint is a directory copy under `documents/<id>/.checkpoints/<cp>/` — history without any in-memory model change. `diff` runs the existing `frame_diff` machinery between a checkpoint and the live canvas and adds regression signals (silhouette fill-ratio delta, distinct-colour-count creep, contrast delta). This is the safety net that makes high-variance ops (`form`/`quantize`/`relight`/`glow`) safe to attempt at all.

**doc_critique** — `doc_critique(id, frame=0, layer?, region?, against?, checks=['silhouette','banding','jaggies','orphans','contrast','ramp_adherence','pillow_shading']) -> {scores:{check:{value,verdict,worst_cells}}, summary}`
A thin **orchestrator** that *calls* the canonical detectors (don't re-implement: route banding→`doc_form_audit`/banding math, jaggies/orphans→`doc_smooth_edges`/components, ramp→`ramp_validate`/`palette_report`). Returns a per-axis verdict plus worst-offending coordinates so the agent can act locally. Ship conservative verdicts first (false "defects" make a blind agent obediently wreck deliberate choices) and add a `material`/`intent` hint later so a steep steel ramp or a one-shot tag isn't flagged.

**doc_transform_cel** — `doc_transform_cel(id, layer, frame, source='cel'|'selection'|region, rotate_deg, scale_x, scale_y, skew_x_deg, skew_y_deg, pivot='center'|[x,y], anchor=[x,y], preserve_volume=false, method='rotsprite'|'nearest'|'area', snap_palette=false, clear_source=false) -> {placed_bbox, orphans_created, off_palette_added}`
Generalizes `rotate_nn`'s inverse-sample to a full affine matrix over a `copy_region` buffer, scoped by `apply_masked`, with RotSprite (upscale→edge-trace→rotate→area-downsample) as the default to avoid shattered clusters. `preserve_volume` derives `sy=1/sx` for squash-and-stretch; `snap_palette` re-quantizes the transform fringe. Returns numeric damage counts so the blind agent gets feedback without a render — and pair it with the transform-audit metrics inside `doc_critique`.

**doc_layer_ops** — `doc_layer_ops(id, action='move'|'insert_at'|'delete'|'rename'|'duplicate'|'merge_down', index?, to_index?, name?) -> {layers}`
Pure manipulation of the `LayerMeta` Vec + `cels` HashMap re-keying; no raster math. `merge_down` flattens two cels via the existing `composite` and is the pragmatic substitute for "light the whole multi-layer character as one silhouette." This is the most-repeated probe wish across all three runs and the cheapest structural win.

**doc_make_perceptual_ramp** — `doc_make_perceptual_ramp(base, count, value_lo, value_hi, hue_shift, sat_curve='flat'|'arc'|'sat-in-shadow', anchor_midtone=false, set_doc?, validate=false) -> {colors, hex, validation?}`
Refines `make_ramp` (raster.rs:721) into OKLCh: equal perceptual-lightness steps, a chroma arc (mids most saturated), and an exact midtone anchor — the three things linear-HSL cannot do. Adds two conversion fns to raster.rs (`oklab`/`oklch`) that the whole palette family (`harmony_palette`, `gradient_map`, `palette_lerp`, `snap_palette`, `colorblind_check`) reuses. Inline `validate` runs `ramp_validate` in OKLab so the agent confirms evenness without a second call.

**doc_smooth_edges** — `doc_smooth_edges(id, layer, frame, mode='selout'|'contour'|'both', ramp?, strength=1.0, max_corner_run=3, only_color?, region?) -> {pixels_added}`
Detects staircase corners on every colour boundary (outer silhouette *and* interior contours), inserts one in-ramp **opaque** pixel per inner corner (ramp-derived, never the current fixed 110/255 translucent grey), and skips long straight runs via `max_corner_run`. Reuses `outline_cel`'s neighbour scan, `pixel_perfect`'s L-detection, and `make_ramp` for the AA colour; honours `apply_masked`. This single op closes the largest craft gap in the engine.

**doc_select_render** — `doc_select_render(id, layer?, frame?, scale=4, mode='rubylith'|'ants'|'mask', report=true) -> {png_path|inline, selected_pixels, bbox, holes, components}`
Renders the existing `Vec<bool>` mask as a Quick-Mask overlay using the same dim-art+flag renderer as `seam_report`/`frame_diff`. Zero model change, tiny surface, and it ends the fatal loop "select blind → paint blind (no undo) → discover the mask was wrong." Should ship inline-image-first, alongside `doc_look`.

**doc_select_wand** — `doc_select_wand(id, layer, frame, x, y, tol=16, connectivity=4, sample='layer'|'flattened', mode='replace') -> {selected_pixels, bbox, region_count}`
Wires the existing 4/8-connected `bucket_fill` flood (document.rs:754) to *write the mask* instead of paint, combined via `doc_select`'s mode logic, with perceptual (luma+chroma) tolerance and composite sampling. It's the contiguous magic-wand the roadmap promises and the precondition for nearly all local recolour/re-shade work.

## 4. Architectural unlocks

Four model changes gate whole tool families. Ranked by leverage-per-cost:

1. **Inline tool-result images** (near-zero cost — the path exists). `rmcp` Content supports image blobs and the resource already encodes them. This is not a model change so much as wiring, but it unblocks `doc_look`, `doc_contact_sheet`, `doc_select_render`, every overlay-emitting audit, and `doc_render_ref`. *Do this first; it multiplies everything.*

2. **OKLab/OKLCh colour space** (two conversion fns in raster.rs). Unlocks perceptually-even ramps, correct `quantize`/`snap_palette`, true ΔE (CVD checks, fuzzy recolour, style cohesion), and gradient-map grading. Without it, the entire colour dimension stays structurally below world-class.

3. **Layer lifecycle + a weighted (f32) selection mask.** Layer ops are a small Vec/HashMap change that unblocks z-order compositing, limb isolation, and whole-figure effects. Upgrading `Selection.mask` from `Vec<bool>` to `Vec<f32>` (and teaching `apply_masked` to lerp by weight) is the one larger change that unlocks feathered selection, soft compositing, and graft-without-hard-seams across the whole surface.

4. **A part/rig data layer** (named parts + per-part pivots on `DocMeta`, optionally bones). This is the only change that unlocks the entire cutout-animation family (`define_part`/`stamp_part`/`pose_keyframe`/`ik_pose`) — pose-to-pose with arcs, follow-through, planted-foot IK, paper-doll part-swap, and the LPC/Spine modular economy. It's the deepest change and correctly deferred until Tiers 1–2 land, but it is the gate between "frame-painting tool" and "animation tool." A lighter precursor — `doc_region_clear(region)` and "lift sub-rect to new layer" — buys 80% of limb-isolation value at a fraction of the cost.

*Deliberately not undertaken:* draw-by-palette-index as a full indexed-colour buffer. `doc_snap_palette` plus OKLab quantize gives most of the cohesion benefit without re-architecting cels away from RGBA.

## 5. What we'd build first

**PR 1 — "Open the agent's eyes" (Tier-1 perception + safety, all low-risk reuse).**
- Wire inline image content into tool results; ship **`doc_look`** (folding in value/sat/hue modes, grid, coords) and **`doc_select_render`**.
- Ship **`doc_checkpoint`** (directory snapshot/restore/diff).
- Ship **`doc_render_value_scoped`** (region+layer+subject-vs-bg histogram) — it's a small param addition that fixes the still-life probe's "85% in the darkest band is just the background" blindness.

This PR changes no data model, reuses the existing render/flatten/frame_diff/base64 machinery, and immediately lifts the agent from "slow, blind, and unable to revert" to "fast, sighted, and safe to experiment." It is the precondition for every craft tool that follows.

**PR 2 — "Structural backbone + first craft loop."**
- Ship **`doc_layer_ops`** (move/insert/delete/rename/duplicate/merge_down) — the most-repeated probe wish, pure Vec/HashMap work.
- Add **OKLab/OKLCh** to raster.rs and ship **`doc_make_perceptual_ramp`** + **`doc_snap_palette`** on top of it.
- Ship **`doc_smooth_edges`** (selout) and **`doc_select_wand`**, and the orchestrator **`doc_critique`** that scores banding/jaggies/orphans/ramp-adherence against a checkpoint.

After these two PRs the agent can *see* what it makes, *revert* what it breaks, *reorder/merge* layers, build a *perceptually-even on-palette* ramp, *anti-alias* its edges, and *self-critique against its last checkpoint* — the closed draw→render→audit→fix loop that produces world-class work. `doc_transform_cel` (the biggest single craft primitive) follows immediately as PR 3 once RotSprite resampling is implemented.

---

**Source notes for the maintainer:** inline base64 image path confirmed at `src/server.rs:1067` (`base64`) and `:1368` (resource blob) — present but not on any tool. Document store at `~/.atelier/documents/` holds **304 docs** including the versioned/family clusters (`amplifier`/`-hd`/`-p`/`-v3`, `combiner-*`, `cat-*`, `plat2-*`) that confirm the set-cohesion blind spot: nothing in the current surface can audit them as one game.
