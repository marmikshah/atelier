# atelier — Drawing-Quality Roadmap (competent-hobbyist → world-class)

> 19-agent forensic + empirical + design workflow (2026-06-19): 6 code readers,
> 4 fresh empirical probes that actually drove atelier, root-cause clustering,
> per-cluster solution design, synthesis, and an adversarial completeness critic.
> Mandate: the user reports MCP drawings are "really low quality and choppy."
> Every claim below is verified against current source.

## 0. The complaint, grounded

Empirically reproduced. Rendering real docs and four fresh probes:

| Output | Result | Why |
|---|---|---|
| `slime-bounce`, `world-dusk-v3` (gradient sky + silhouette layers) | **good** | procedural-friendly: gradients + simple silhouettes play to the engine's strengths |
| `swordsman-slash` action arc | **choppy** | curved slash = stacked beziers → disconnected, gappy, frayed segments |
| `goku-kamehameha-v2` (64px) | **soft/blurry** | FX bloom → **388 distinct colours** on a locked palette; reads as AA mush, not crisp pixel art |
| `goku-jump-v3` import | **noisy photocopy** | grayscale, mottled mid-tones — import pipeline loses crispness + saturation |
| fresh probes (slash / walk / orb / bust) | all **mediocre** | see root causes below |

The pattern: *procedural-friendly subjects succeed; ambitious strokes, action, FX and imports degrade.* The ceiling is structural, not effort.

## 1. Verdict

atelier renders **every** stroke through one primitive — a constant `size×size`
square brush (`raster.rs:19`) stamped along an integer Bresenham path
(`raster.rs:29`) — and **never re-quantises the continuous-tone output of any
FX** (`snap_to_palette` even preserves source alpha, `document.rs:563`). That is
the whole complaint: choppy staircased edges, organic strokes faked as gappy
stacked beziers, FX that blow an 8-colour palette into 140–388 colours.

The single biggest *capability* lever is a **coverage-based signed-distance
stroke core with width profiles** (F1) — one rasterizer that makes tapered,
connected, anti-aliased, palette-snapped strokes *by construction*. The single
biggest *cheap* lever is **alpha-aware palette snap + auto-snap-after-FX** (F2),
which is ~one policy change because `snap_to_palette` already uses OKLab
`PaletteLab`. The biggest *missed* lever (per the critic) is a **per-pixel error
map + local-revision loop** (C7) — the agent gates on aggregate numbers and
cannot see *which pixel* is wrong; fixing the loop beats any single new stroke.

All the plumbing the fixes need already exists, unused: `srgb_to_linear`
(`raster.rs:487`), `srgb_to_oklab` (`:507`), `nearest_oklab` (`:567`),
`PaletteLab` (`:588`), `composite_px` (`:300`), `over` (`:414`),
`interior_distance` (for radial falloff), `make_ramp_oklch` (`:644`).

## 2. Root causes (ranked, all source-verified)

| # | Cluster | Severity | Killer evidence |
|---|---|---|---|
| **C1** | No variable-width / tapered / fillable curve — organic strokes impossible by construction | critical | `brush()` constant square (`raster.rs:19-26`); `pencil` never interpolates between points; probe crescent via stacked beziers = "disconnected, gappy, frayed", via polygon = "tip ballooned" |
| **C2** | Continuous-tone FX pollute the locked palette; `snap_to_palette` can't fix it (preserves alpha) | critical | `snap` writes `[c0,c1,c2, p.0[3]]` (`document.rs:563`); one `doc_glow` turned 8 → 140 colours; `box_blur` averages in **gamma** sRGB (`raster.rs:349`, no `srgb_to_linear`) |
| **C4** | Animation has no shape interpolation; rigid-rotate shatters/fringes at pixel scale | critical | `affine_nn` NN-sample + Triangle downscale (`raster.rs:725`); one 25° leg rotate = 3→66 colours / 63 fringe pixels; legs scissor (single pivot) |
| **C6** | No AA curve rasterizer; bezier double-quantizes → raw staircase | high | `draw_line` pure integer Bresenham; `bezier` rounds every sample before stitching (`document.rs:1350`); single-tier `smooth_edges` left 38 step-corners on the bust |
| **C3** | On-palette softness impossible: only quantised falloff is per-pixel dither → orphan speckle | high | radial+bayer held palette at 8 but rendered 41 orphan-speckle cells `doc_critique` flags as noise; no banded/stepped falloff |
| **C5** | No figure/skeleton construction — LLM emits every silhouette vertex blind → stiff blocky stacks | high | only recipe is "block with rect/ellipse/polygon" (`SKILL.md:122`); probe arm "reads detached from torso"; no capsule/limb/arc primitive |
| **C7** | *(critic add)* No first-class local-revision loop / per-pixel error map — agent can't see-and-repair individual pixels | high | `doc_critique`/`doc_components` return **aggregate** numbers; a 0.95 IoU hides exactly the 5% a master fixes first; agent forced to "emit whole silhouette blind" |

## 3. The roadmap (deduped, ranked by quality-per-effort)

| Fix | What it does | Quality leap | Effort | Leverage | Builds on |
|---|---|---|---|---|---|
| **F2 — Alpha-aware snap + auto-snap-after-FX** | snap policy `opaque`/`flatten` (threshold or composite-over-backdrop then OKLab-snap to opaque); opt-in `snap` on glow/blur/drop_shadow/gradient/relight | FX stop blowing the palette (140→8); "soft/blurry" cured | **S–M** | **transformational** | `PaletteLab`, `over:414` — snap already OKLab |
| **F1 — SDF coverage stroke core** (`doc_stroke`/`doc_capsule`) | one f32 rasterizer: min-distance-to-capsule-union, coverage = `clamp(halfwidth(t)+0.5−d,0,1)`, composited over the backdrop then snapped | organic/tapered strokes connected + AA by construction; kills "choppy disconnected arcs" | L | **transformational** | `composite_px`, `nearest_oklab`, `PaletteLab`, `srgb_to_linear` |
| **F-loop — Per-pixel error map** (`doc_ref_compare`/`doc_critique` per-cell grid + signed silhouette-diff + worst-pixel rects) | agent sees *which* pixels are wrong and in which direction | converts "draw blind, gate on aggregate" → "draw, see error, fix locally" | **S** | **high** | `doc_ref_compare` ΔE already computed; `dump_region` |
| **F8 — Banded glow** (`doc_glow style:"banded"`) | concentric ramp rings (ring index = `round((1−t)(n−1))`) → solid on-palette bands, zero orphans | smooth bloom *and* crisp on-palette become compatible | M | high | `form` radial loop, `composite_px`, `parse_blend` |
| **F5 — Catmull-Rom pencil/polyline** (`smooth`) | interpolate sparse points through a centripetal Catmull-Rom spline routed through F1 | sparse strand/wisp points → one connected run, not floating dabs | M | high | F1; `lerpf:803` |
| **F6 — Adaptive de Casteljau bezier** | recursive flatness subdivision, f32 samples, single F1 pass | kills doubled corners / corner-cutting on tight bends | M | high | f32 `at(t):1322`; F1 |
| **F9 — OKLab relight index + on-palette `smooth_edges`** + hue-shifted ramps | index relight by OKLab L not luma; AA midpoints land on-palette; ramps shift hue (warm light / cool shadow) | shading hits the right step; AA fringe on-palette; chromatically alive | M | high | `srgb_to_oklab`, `make_ramp_oklch`, `nearest_oklab` |
| **F10 — Multi-tier iterative selout** (`smooth_edges passes=2-3`) | fixed-point loop, widen notch window to 2–3px runs, snap each tier | steep curves fully resolve (bust 38→≤8 step-corners) | M | high | single-tier fill `document.rs:835` |
| **F7 — Linear-light blur** | `srgb_to_linear` LUT before blur accumulate | soft FX physically correct (no dark fringes) | M | medium | unused `srgb_to_linear:487` |
| **F-keyframe — Sub-pixel motion accumulation** | carry fractional offset across eased frames (monotone rounding) | eased translate stops stuttering | **S** | medium | `keyframe_move` |
| **F4 — RotSprite rotation** | Scale2x/EPX upscale → NN-rotate → majority-vote downscale | rotated limbs stop shattering (66→≤3 colours) | L | medium *(demoted)* | `affine_nn:725` |

**Critic's demotion of F4:** masters don't rotate finished sprites — rotating raster art always shatters, which is why the rig regenerates limbs from joints (B2). F4 is L-effort polish on a path good animation avoids; its own acceptance test ("every pixel an input colour or transparent") bans the very coverage-AA F1 makes the house style. Ship the free sub-pixel rounding now; do F4 only for whole-prop spins, after the rig.

## 4. Build first — PR-sized, in order

Reconciled order (construction + hygiene + loop first; critic's reorder applied — F2 before F1 because it's nearly built and kills the largest *measured* regression):

1. **F2 — alpha-aware snap + opt-in FX snap.** *Smallest, highest measured impact, independent.*
   *Touch:* `Document::snap_to_palette` alpha-policy enum (`document.rs:542`); `Studio::snap_palette` + `DocSnapPalette` params (`craft.rs:570`, `server.rs:1179/2950`); opt-in `snap` on `glow` (worst offender) then the other FX.
   *Acceptance:* re-run the glow-orb probe with `snap` → `doc_palette_report distinct == palette_len`, `off_palette == 0`, silhouette IoU vs pre-snap ≥ 0.95; no-palette docs byte-identical to today.

2. **F1 — SDF coverage stroke core + `doc_stroke`/`doc_capsule`.** *The substrate every C1/C5/C6 fix routes through.*
   *Touch:* `raster::stroke_coverage(w,h,&segments,profile)` + `fill_capsule`; `Document::stroke`/`capsule`; tools mirroring `doc_polygon`; batch ops `stroke`/`capsule`.
   *Acceptance:* crescent from 6-pt centerline, widths `[0,3,5,5,3,0]` → `doc_components` reports **exactly 1** component (vs >1 for stacked beziers); mid cross-section 9–11px, tips 1px; 45° capsule → ≥20 fractional-coverage edge pixels (Bresenham yields 0); with palette locked, `distinct == palette_len`.

3. **F-loop — per-pixel error map.** *Fixes the iteration loop, not the stroke — the critic's cheapest transformational win.*
   *Acceptance:* `doc_ref_compare`/`doc_critique` returns a per-cell ΔE grid + worst-pixel coordinate list the agent can feed straight into a local fix.

4. **F8 — banded glow** + **F5/F6 — smooth pencil/bezier** (ride on F1).

## 5. Bigger bets (architectural — gate after the core lands)

- **B1 — Skeleton-to-figure** (`doc_figure`): agent supplies joint pixel coords + width hints; each bone fleshed as an F1 capsule sharing endpoints → one connected silhouette by construction. Lets the LLM reason in *joint space* (which it can) not silhouette vertices (which it can't). **Gate:** F1 first. **The named missing tool.**
- **B2 — 2-bone analytic IK** (`solve_ik2`, law of cosines) + `pose_limb` → walk cycles *generated* from joint tables, not hand-repainted (the textbook source of inter-frame wobble). **Gate:** B1.
- **B3 — Rig-first walk-cycle workflow** + shared static torso/head layer + `all_frames` cleanup flag. Guidance + small tool; **gate after B1/B2** so the recommended path actually works.

### Master-grade gaps the critic flags as the *next* ceiling (beyond this roadmap)
- **Centerline-only is a balloon-animal ceiling.** Capsule-union limbs give smooth tubes with convex joints; you cannot get a sharp elbow, concave underarm, pointed jaw, or occlusion seam. World-class needs **edge/contour authoring**, not only medial-axis sweeps.
- **No light-source model.** Fixing blur/ramp plumbing (F7/F9) doesn't decide *where light comes from*: no normals, no consistent key direction across parts, no AO at joint seams, no terminator placement → flat plastic. **Hue-shifted ramps** (warm light / cool shadow) are mandatory for "painted, not shaded."
- **Coverage-AA can become the AI-mush tell.** Snapping a distance-field fringe per-pixel risks the orphan-speckle look. Mitigation baked into F1: composite coverage over the *actual backdrop* then snap, so AA is contiguous along the contour, not scattered.

---

*The rule across everything: ship the clean-by-construction primitives (F1/F2) and
the revision loop before any rig — a perfect IK solver still produces stiff
output if its limbs rasterize through the old square brush.*
