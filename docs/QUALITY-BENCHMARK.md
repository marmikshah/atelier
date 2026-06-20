# atelier — Quality Benchmark (v1.1.0 → v1.2.0)

A **deterministic** measure of the engine's capability lift: for each capability,
draw the thing the OLD way (primitives that still exist) and the NEW way, then
score both on objective atelier metrics. No agent, no randomness — fully
reproducible, and it runs in CI as a regression guard.

- **Source:** `quality_benchmark` test in `src/studio.rs` (asserts new > old on
  every axis).
- **Run the table:** `cargo test --release quality_benchmark -- --nocapture`

## Result

| Capability | Metric (direction) | v1.1.0 way | v1.2.0 tool | Δ |
|---|---|--:|--:|---|
| Tapered slash arc | stroke tip width, px (lower = tapers) | 3 | 2 | thinner tip |
| Stroke edge | anti-aliased pixels (higher = smooth) | **0** | **82** | hard staircase → smooth edge |
| FX glow on an 8-colour locked palette | distinct colours (lower = crisp) | 42 | **2** | off-palette bloom → on-palette |
| Lit form (sphere) | value steps (higher = real volume) | 1 | **5** | flat fill → shaded form |
| Shading ramp | OKLab L-step spread (lower = even) | 0.039 | **0.001** | ~39× more perceptually even |

## What each row compares (apples-to-apples, same binary)

1. **Slash arc** — `doc_polyline` (uniform square brush) vs `doc_stroke` (width
   profile `[1,6,6,1]`). Tip column pixel count + count of fractional-alpha
   (anti-aliased) pixels. Old = uniform 3px tip, **zero** AA (Bresenham); new =
   tapering tip + a smooth analytic edge.
2. **FX glow** — `doc_glow` with the snap off (v1.1.0 behaviour) vs the new
   default on, both on the same 8-colour locked-palette sprite. Distinct opaque
   colours in the result: the old bloom blew the palette to 42; the new path
   re-snaps to **2** on-palette colours.
3. **Lit form** — a flat-filled ellipse vs the same ellipse run through
   `doc_form` + `doc_rim_light`. Distinct colours = tonal steps: **1** (flat) →
   **5** (full ramp + rim).
4. **Shading ramp** — a naive linear-RGB lerp (dark→light) vs `doc_palette`
   `scheme=mono` (OKLCh). Spread between consecutive OKLab-L steps; lower is
   more perceptually even. The RGB ramp crushes the midtones (spread 0.039); the
   OKLCh ramp is near-perfectly even (0.001).

## Scope + honesty

- This measures the **tool / engine capability lift** deterministically — what
  `doc_stroke`/`doc_figure`/`doc_palette`/snap/`doc_form`/`doc_rim_light` give
  over the v1.1.0 primitives. It does **not** measure end-to-end *agent* art
  quality (that needs an agent drawing a task suite + a perceptual judge, which
  has sampling variance) — that's the separate end-to-end eval, still open.
- The atelier metrics (palette discipline, AA, value steps, ramp evenness)
  measure **craft discipline**, not beauty. The perceptual "last 5%" is what
  `doc_diff_map` (shipped) and a vision judge (deferred `doc_critique_vision`)
  address.
- New benchmark rows are cheap to add: pick a capability, draw old-vs-new, score
  a metric, assert new > old.
