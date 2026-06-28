# Changelog

All notable changes to atelier are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow
[SemVer](https://semver.org/).

## [1.2.0] — 2026-06-20

The drawing-quality release: the engine that was choppy and palette-blowing now
draws connected, tapered, perceptually-shaded, palette-true art — and the agent
can finally *see* its own error to repair it. 105 tools, 198 tests.

### Added

- **Tool profiles** (`ATELIER_PROFILE`). The server advertises a **core** profile
  of ~28 tools by default — the canonical sprite / animation / tile /
  recreate-from-reference workflow — and the full 65 when `ATELIER_PROFILE=full`.
  The profile filters `tools/list` (discovery) only; `call_tool` still routes
  every tool, so `atelier replay` and recipes always reach the long tail. Cuts
  the context the model loads without folding rich tools into worse shapes.
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
- **`doc_critique_vision`** — the AI eye for free-form art (no reference): renders
  the frame and asks the MCP host to run its own vision model over it, returning a
  structured critique (reads-as, silhouette/proportion, value/colour, top-3 fixes).
  Ethos-pure — atelier ships no weights, makes no network call, holds no keys; the
  host samples. Requires a host advertising the `sampling` capability; errors
  clearly (no hang) if absent.
- **`doc_palette`** — one OKLCh generator for a single shading ramp
  (`scheme="mono"`) or a multi-hue scheme (complementary / triadic / analogous /
  split / tetradic), with hue-shift, saturation curve, midtone anchor and
  evenness validation.
- **Interactive `/playground`** web view (served by `atelier --http`): a tool
  list with an auto-built form per tool (from its live JSON schema) and a live
  canvas — plus a **draw mode** where mouse gestures *are* tool calls
  (pencil/eraser → `doc_pencil`, line/rect/ellipse drag → the matching tool,
  click → `doc_fill`). No Node, no external assets, rides the existing `/mcp`.
- **Live render** — `/gallery` and `/playground` update the instant the agent
  edits a document, via a Server-Sent Events stream (`/gallery/events`) the server
  pushes a `{doc, tool, args}` event to on every successful mutating tool call. No
  more 2.5s polling lag (the poll stays only as a fallback).
- **`/live`** — a focused single-document session view: pick a doc, then watch the
  canvas re-render AND a live feed of the tool calls hitting it (name + compact
  args) stream in real time. For watching an agent draw — no tool forms, no editing.
  A freshly created doc (`doc_create`, whose id comes from the result not the args)
  is broadcast too and auto-attaches the view, so you can open `/live` empty and
  watch the agent start from scratch.
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
- **Shape tools reframed as blocking** — `doc_ellipse`/`doc_polygon` descriptions
  and the 2d-game-art skill now state a stamped shape is a *base*, never a finished
  sprite, and mandate the volume + pixel-detail + polish pass (the fix for clumsy
  stamped art); ≥48px canvases for detailed characters.
- Tighter input validation and correctness across the surface (perspective
  vanishing-point, `doc_move_region` source-over, `doc_select_wand` flood guards,
  `doc_create` dimension clamp, blend/direction/`copy_from` validation).

### Removed

- **`doc_export_gif`**, **`doc_export_apng`** (→ `doc_export_anim`),
  **`palette_ramp`**, **`doc_make_perceptual_ramp`**, **`doc_harmony_palette`**
  (→ `doc_palette`). Hard removal — no deprecated aliases.
- **`doc_bezier`** (gappy, hard-staircased — `doc_stroke` supersedes it) and the
  **`doc_get_pixel`** *tool* (a strict subset of `doc_dump_region` 1×1; the
  internal pixel reader stays). Renamed **`doc_tween` → `doc_dissolve`** — it
  cross-fades, it never interpolated poses, so the old name was a footgun.
- **`doc_render`** and **`doc_render_value`** → folded into **`doc_look`** (the
  deferred round-2 consolidation). `doc_look` absorbs `tile` (N×N seam check) and
  `out_path` (file write) from `doc_render`, and `band_pcts` per-band coverage in
  `bands`/`notan` modes from `doc_render_value` — so one SEE call covers render +
  every analysis channel, killing the three-way "show me the canvas" name clash.
  Hard removal, no aliases.
- **Round-3 op-dispatch fusion.** The cel-mutation tools fold into two op-dispatch
  tools over the same validated `apply_op`/`batch_op_keys` path, every op's params
  verified identical (no capability lost); the studio methods stay as library API.
  - **`doc_draw`**`(op, …)` — the 13 *add-a-mark* tools (`doc_pencil`, `doc_line`,
    `doc_rect`, `doc_ellipse`, `doc_polyline`, `doc_polygon`, `doc_stroke`,
    `doc_fill`, `doc_fill_cel`, `doc_gradient`, `doc_scatter`, `doc_noise`,
    `doc_text`).
  - **`doc_fx`**`(op, …)` — 14 *rework-existing-pixels* tools (`doc_blur`,
    `doc_outline`, `doc_drop_shadow`, `doc_bevel`, `doc_shade`, `doc_form`,
    `doc_dither`, `doc_pixel_perfect`, `doc_flip`, `doc_shift`, `doc_symmetry`,
    `doc_quantize`, `doc_replace_color`, `doc_adjust`). `doc_glow` stays separate —
    its on-palette `snap` isn't a batch op.

  - **`doc_export`**`(op, …)` — the 3 per-document file exports (`doc_export_sheet`,
    `doc_export_anim`, `doc_export_tileset`) over a shared `out_path`/`scale` core.
    (`doc_wang_tiles`, `export_all`, `export_atlas` stay separate — generators /
    library-level.)

  - **`doc_layer`**`(op, …)` — `doc_add_layer` + `doc_set_layer` + `doc_layer_ops`
    folded into `add`/`set`/`move`/`insert`/`delete`/`rename`/`duplicate`/`merge_down`.
  - **`doc_frame`**`(op, …)` — `doc_add_frame` + `doc_set_frame_duration` +
    `doc_frame_ops` folded into `add`/`duration`/`insert`/`duplicate`/`delete`/`move`.
    (Pivots, boxes, tags and keyframe motion keep their own tools.)
  - **`doc_region`**`(op, …)` — `doc_copy_region` + `doc_cut_region` + `doc_paste`
    + `doc_move_region` + `doc_clear_region` folded into `copy`/`cut`/`paste`/`move`/`clear`.
    (`doc_stamp_image` and `doc_extract_to_layer` keep their own tools — image
    import and rigging; `doc_select`/`doc_select_wand` stay separate per round-2's
    wand-tolerance trap.)
  - **`doc_ref`**`(op, …)` — `doc_set_reference` + `doc_import_clean` folded into
    `set` (attach the comparison reference) / `import` (trace a cleaned image onto
    a guide layer). The reference *readers* (`doc_ref_analyze`, `doc_ref_compare`,
    `doc_diff_map`) stay discrete.

  See `docs/CONSOLIDATION-ROUND3.md`. **65 tools** (from 105 at the start of the
  refactor — the writer surface fused into op-dispatch tools, readers kept discrete).

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
