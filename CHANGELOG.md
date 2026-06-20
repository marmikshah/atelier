# Changelog

All notable changes to atelier are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow
[SemVer](https://semver.org/).

## [1.2.0] — 2026-06-20

The drawing-quality release: the engine that was choppy and palette-blowing now
draws connected, tapered, perceptually-shaded, palette-true art — and the agent
can finally *see* its own error to repair it. 104 tools, 198 tests.

### Added

- **`doc_stroke`** — clean stroke core: an SDF capsule-union ribbon, per-vertex
  width (taper to a 1px tip), anti-aliased and connected by construction.
  Replaces the old constant-square-brush Bresenham path for smooth curves/limbs.
- **`doc_figure`** — build a connected humanoid from named joint coordinates;
  each bone a tapered capsule sharing endpoints, so one silhouette by construction.
- **`doc_walk`** — generate a side-view walk cycle from a standing pose with
  2-bone analytic IK (law of cosines), counter-swinging arms and a body bob.
- **`doc_diff_map`** — per-pixel signed OKLCh error map vs the stored reference:
  a heat PNG (red = too light, blue = too dark, green = wrong hue) plus the worst
  individual pixels, each with a fix direction (lighten/darken, saturate, shift
  hue). The see-and-repair eye for the perceptual last 5%.
- **`doc_rim_light`** — outward-normal edge light from an azimuth; `dark=true`
  paints the away-facing contact shadow. Topological, survives tiny canvases.
- **`doc_palette`** — one OKLCh generator for a single shading ramp
  (`scheme="mono"`) or a multi-hue scheme (complementary / triadic / analogous /
  split / tetradic), with hue-shift, saturation curve, midtone anchor and
  evenness validation.
- **Interactive `/playground`** web view (served by `atelier --http`): a tool
  list with an auto-built form per tool (from its live JSON schema) and a live
  canvas — plus a **draw mode** where mouse gestures *are* tool calls
  (pencil/eraser → `doc_pencil`, line/rect/ellipse drag → the matching tool,
  click → `doc_fill`). No Node, no external assets, rides the existing `/mcp`.
- **Quality benchmark** (`quality_benchmark` test, `docs/QUALITY-BENCHMARK.md`):
  a deterministic, agent-free measure of the v1.1.0→v1.2.0 engine lift that runs
  in CI as a regression guard.
- **CI guard** that fails the build on local machine paths in tracked files.

### Changed

- **`doc_export_anim`** (`format="gif"|"apng"`) fuses and supersedes the separate
  GIF/APNG export tools; snaps all frames to one shared palette to kill flicker.
- **RotSprite resampling** for rotation (`doc_transform_cel`,
  `doc_keyframe_transform`, `doc_stamp_image`): EPX upscale → transform →
  majority-vote downscale, so a rotate emits only source colours (no NN fringe).
- **FX palette hygiene** — continuous-tone FX (`doc_glow`, `doc_gradient`)
  auto-snap on-palette when a palette is locked; `doc_relight` rounds forms
  (blurred interior-distance height field, no facet creases); `doc_burst`
  dissipates toward the rim instead of darkening solid.
- **OKLCh ramps are gamut-mapped** — out-of-sRGB steps reduce chroma (binary
  search) while preserving lightness and hue, instead of per-channel clipping
  into a hue shift. Affects `doc_palette` / `doc_relight` / `doc_shade` /
  `doc_form` / `doc_dither_ramp`.
- **Alpha-aware palette snap** (`AlphaSnap` Preserve/Opaque/Flatten); `doc_figure`
  / `doc_walk` snap opaque for crisp edges.
- `doc_get_pixel`'s `layer` is now optional — omit it for the flattened composite.
- Tighter input validation and correctness across the surface (perspective
  vanishing-point, `doc_move_region` source-over, `doc_select_wand` flood guards,
  `doc_create` dimension clamp, blend/direction/`copy_from` validation).

### Removed

- **`doc_export_gif`**, **`doc_export_apng`** (→ `doc_export_anim`),
  **`palette_ramp`**, **`doc_make_perceptual_ramp`**, **`doc_harmony_palette`**
  (→ `doc_palette`). Hard removal — no deprecated aliases.

### Fixed

- Triaged a 69-agent adversarial review of the new sight/light tools:
  `doc_rim_light` (i32 overflow, false rim on full-bleed edges, hard stamp
  punching a translucent hole → composite-by-strength); `doc_diff_map`
  (chroma-weighted ΔH, one dominance rule so the heat colour and the named fix
  agree, + silhouette IoU / scale); `doc_walk` (world-anchored IK — no more
  mid-stride knee inversion).
- `affine_nn` 90° rotation is now a clean permutation (centre-sampled) with a
  bounded allocation cap.
- Flood/`replace_color` tolerance is RGB-only (`close_rgb`) — no more
  anti-aliasing halos from alpha leaking into the channel distance.

## [1.1.0] and earlier

See the GitHub releases for [v1.1.0](https://github.com/marmikshah/atelier/releases/tag/v1.1.0),
v1.0.1 and v1.0.0.
