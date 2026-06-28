# atelier — Full Tool-Surface Audit (103 tools)

> 17-agent per-tool audit (2026-06-20, on `dev/v1.2.0`): 15 readers traced every
> tool's real Rust path (server → studio → document/raster/analysis), scored
> correctness · output-quality · description-accuracy · defects · redundancy,
> then a synthesis + an adversarial critic. avg output_quality **3.96/5**.

## Surface health

Broadly strong, pinned to "competent" by a **small set of shared cores leaking
through many tools**, not by breadth or correctness. ~12 world-class (`doc_form`,
`doc_frame_diff`/`frame_ops`, `doc_shift`, `doc_drop_shadow`, `doc_dump_region`,
`doc_paint_grid`, `doc_stroke`, `doc_snap_palette`, `doc_translucency_report`,
`doc_export_apng/sheet/tileset`, `doc_make_perceptual_ramp`); ~70 solid `q4` with
one low-severity edge; 18 buggy; 2 broken modes. The feedback/analysis half and
the perceptual-colour spine are the strongest things in the codebase. Fix ~7
shared cores and 20+ tools jump a tier at once.

## Bug backlog (correctness != ok or high/critical defect)

| Tool | Problem | Where | Fix |
|---|---|---|---|
| **doc_perspective_guide** | `vp` rays cast `(1.0 - t.fract())` to i32 *before* ×width → all bottom rays collapse to x=0; mode ~25% broken | `craft.rs` vp branch | `((1.0-t.fract())*(w-1) as f32) as i32` |
| **palette_ramp** | legacy linear-HSL `make_ramp` crushes midtones → near-black mud; OKLCh engine sits right beside it | `raster.rs` make_ramp | reimplement on `make_ramp_oklch` (or retire — see cuts) |
| **doc_noise** | dither hardcoded `"none"` → continuous 400-colour lerp, blows locked palette | `document.rs:2494` | plumb a `dither` param into `sample_gradient` |
| **doc_move_region** | overwrite-paste erases a rectangular hole of surrounding art at the destination | `document.rs:750→707` | paste source-over (only opaque pixels write) |
| **doc_select_wand** | `perceptual=true` → `oklab_delta` drops alpha → wand on a fill bleeds into transparent bg (ΔE 0) | `raster.rs:649-652` | make `oklab_delta` alpha-aware **(critic: do this first — shared kernel)** |
| **doc_burst** | shockwave fades toward the DARK ramp end and stays fully opaque → reads as *darkening*, not dissipating (inverted) | burst ramp/alpha | fade alpha + brightness toward the rim |
| **doc_batch** | per-op `opacity` collides with composite-opacity; `drop_shadow`/`form` missing from `batch_op_keys` | `document.rs:3689,4195` | route per-op opacity to the op; register the ops |
| **doc_replace_color** | tolerance = sum of \|Δ\| over 4 channels incl. alpha (~4× stronger, ≠ doc_fill); AA halo left | `raster.rs:227` | RGB-only per-channel tolerance (NOT a blind `close()` rewrite — it has callers) |
| **doc_set_layer / doc_add_layer** | `blend` free-form string, no validation → typo silently composites Normal, `ok:true` | `raster.rs:328` parse_blend | validate vs the blend set, error with the valid list |
| **doc_add_tag** | `direction` unvalidated → `ping-pong`/`rev` silently play forward | `document.rs:3285` | validate forward/reverse/pingpong |
| **doc_add_frame** | `copy_from` out-of-bounds = silent no-op → blank frame (breaks walk-cycle dup) | add_frame | validate `copy_from < frames.len()` |
| **doc_create** | no w/h validation: 0-px accepted (downstream divide), 100000² → OOM | `studio.rs:178` | clamp 1..=4096, else error |
| **doc_clear_region** | off-canvas clear returns bare `{ok:true}`, materialises a blank cel, no rect clamp | `studio.rs:1744` | route through `change_ack`, clamp rect first |
| **doc_components** | specks double-reported (in `specks` *and* `components`); speck scan scoped to colour filter | `analysis.rs:298` | make specks disjoint; scan all opaque |
| **doc_contrast_check** | default `min_ratio=1.5` (below any WCAG tier); region mode means inside+surround before one ratio | `server.rs:2733` | **critic: stop calling it WCAG** — report luma separation, don't chase 4.5 |
| **doc_select_render** | hardcodes `flatten(0)` but mask is doc-global → previews frame 0 while editing frame 3; role colours reversed vs doc | `craft.rs:297` | optional `frame` param |
| **doc_anim_audit** | arc `volume_cv` uses whole-cel mass while centres are region-clipped → mixed scopes | `analysis.rs:1243` | honour region in volume_cv |

## Systemic patterns (cross-tool root causes)

- **P1 — Constant square-brush stroke core** (blocky/off-centre/overwrite-only thick lines): `doc_line`, `doc_polyline`, `doc_bezier`, `doc_rect` outline, `doc_ellipse`. → route `size>1` through the `doc_stroke` capsule core, **as a `round` brush mode, not a wholesale reroute** (a hard square thick line is sometimes the correct pixel aesthetic).
- **P2 — sRGB-HSL math where OKLab exists**: `palette_ramp`, `doc_adjust`, `doc_shade` hue-snap, `doc_ramp_validate`, `doc_panel` bevels. → standardise on the OKLCh path already in `make_perceptual_ramp`/`quantize`/`snap`.
- **P3 — No auto-snap-to-palette on FX/shading** (off-palette drift, manual `snap_palette` chase): `doc_adjust`, `doc_gradient`(none), `doc_noise`, `doc_shade`, `doc_smooth_edges`, `doc_outline` aa, `doc_panel`, `doc_relight`, `doc_glow`(no-palette case). → alpha-aware auto-snap default (mirror the shipped `doc_glow` snap), fallback to cel colours when no palette. **Gated on the `oklab_delta` alpha fix.**
- **P4 — Silent free-form string params degrade to a default**: `blend`, `direction`. → validate vs known set, error with the list.
- **P5 — Missing ack / silent no-op on bad coordinates**: `doc_clear_region`, `doc_clear_cel`, `doc_add_frame`, `doc_move_region`. → route destructive ops through `change_ack`, clamp/validate rects first.
- **P6 — Uneven region/layer/selection exposure**: `doc_get_pixel`/`doc_coverage_map` lack `layer`; `doc_quantize`/`doc_transform_cel`/`doc_outline_selective` ignore the active selection. → `layer` optional+flatten; route FX through `edit_masked`.
- **P7 — NN affine rotation shreds pixels**: `doc_keyframe_transform` (affine_nn), `doc_stamp_image` (rotate_nn), `doc_transform_cel` (4× NN supersample mislabeled "rotsprite"). → one shared edge-preserving/area-weighted resample + alpha-threshold cleanup.
- **P8 — (critic) No shared anchor / half-pixel / edge-inclusivity convention**: top-left vs centre origin, pivot-relative vs absolute, inclusive vs exclusive rect edges, even-brush off-centre. Silent half-pixel drift when rigging a limb across move/keyframe/stamp/pivot. → a documented coordinate contract + one rounding helper.

## Redundancy / cuts

- **`palette_ramp` → delete** (critic: don't keep a strictly-worse twin of `doc_make_perceptual_ramp` — agents will call it).
- **`doc_get_pixel` → fold into `doc_dump_region`** (1×1 case; its headline "verify blind" use is exactly the layer-only case it gets wrong).
- **`doc_polyline`/`doc_bezier` → keep only as constant-width; route organic use to `doc_stroke`.**
- **`doc_render` ⊂ `doc_look(mode=render)`**, **`doc_render_value` ⊂ `doc_look(mode=value/bands)`** — keep `doc_render` only as the file-write path.
- **`doc_move_region`/`doc_cut_region`** are intentional wrappers — keep, but fix the overwrite footgun.

## Prioritized fix list (excludes shipped stroke-core / palette-snap)

**S-effort correctness (clear first):** `oklab_delta` alpha-aware (do first — kernel) · perspective vp bug · string-param validation (blend/direction) · `doc_noise` dither · `doc_move_region` source-over · `doc_create` clamp · destructive-op `change_ack`+clamp · `doc_replace_color` RGB-only tolerance · `palette_ramp`→OKLCh/delete · `doc_burst` un-invert · `doc_select_render` frame param · `doc_get_pixel`/`coverage_map` layer parity · description sync pass.

**M/L quality investments (after the backlog is clean):** P3 auto-snap rollout (M) · `doc_relight` blur the height field + lower default bulge (M) · `doc_export_gif` one global palette (M) · P7 shared edge-preserving rotation (L) · `doc_material` u,v from silhouette bbox (M) · `doc_text` 5×7 font (M).

**Critic's strategic note:** "~70 tools one edge-case from betraying a master" — the move at 103 tools is to **cut surface**, not just clear a 21-item backlog. Trim redundant/footgun tools as aggressively as you fix.
