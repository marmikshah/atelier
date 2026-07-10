# Changelog

All notable changes to atelier are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow
[SemVer](https://semver.org/).

## [1.3.0] — 2026-07-08

The identity + polish release. atelier stands on its own name — **the pixel-art
studio agents can see** — with the last two structural quality gaps closed:
continuous-tone effects can no longer blow the locked palette, and stroke poses
keep sub-pixel precision so walk cycles glide instead of stepping. Adds
the game layer (`doc_set_audit` / `doc_set_palette_sync` — a game is a SET of
documents), `doc_form_audit` (an eye for the #1 shading failure),
`doc_cast_shadow` (a projected ground shadow), engine-standard sheet JSON,
`list_docs` family filters, the `doc_pose_cycle` moveset generator, and the
47-blob autotile family (`doc_autotile_set` + `doc_tilemap_assemble`).
72 tools, 218 tests.

### Added

- **`doc_pose_cycle`** — the moveset generator (full profile). One standing pose
  (the 13 `doc_figure` joints) in, a full tagged animation cycle out, per gait:
  `idle` (breathing bob), `run` (airborne stride, pumping arms, forward lean),
  `jump` (crouch → rise+tuck → fall → landing absorb), `attack` (lead-arm sweep
  with a lunge), `hurt` (recoil and recover). Same 2-bone-IK, connected-capsule
  machinery as `doc_walk`; amplitudes derive from the figure's own leg length ×
  `intensity`, so every preset fits any sprite size. One call per gait = a whole
  character moveset from the same pose.
- **The 47-blob autotile family** (full profile) — terrain the agent can
  finally see assembled.
  - **`doc_autotile_set`** — the deterministic 47-tile blob set (full
    edge+corner bitmask family, the modern superset of the Wang 16) from the
    same inner/outer material contract, into a NEW `<id>-blob` document plus
    `masks` (the canonical neighbour mask per grid index) so engine autotilers
    map straight onto the sheet.
  - **`doc_tilemap_assemble`** — the in-situ test of a tileset: a terrain mask
    (`rows` strings) in, every filled cell rendered from its 8-neighbour mask
    with the same blob rules, out as a NEW `<id>-map` document to `doc_look`
    and export. Closes the last see-and-critique gap: the MAP, not just the
    tile.
- **The game layer.** A game is not one sprite but a set of documents that must
  read as one work — and nothing could see that set until now.
  - **`doc_set_audit`** (core profile) — audit N documents (explicit ids and/or
    an id prefix like `hero-`) as ONE game: per-doc palette/value/scale/pivot
    stats plus set-level cohesion — palette union size, unlocked docs, cross-doc
    near-duplicate colours (OKLab ΔE), silhouette-height scale outliers vs the
    set median, the set value range, and missing pivots. Verdict is `cohesive`
    or a list of actionable warnings.
  - **`doc_set_palette_sync`** (core profile) — broadcast one palette across a
    set: lock it on every member and perceptually snap every cel onto it
    (explicit colours or copied `from_doc`). The one-call fix for set-audit
    palette warnings.
  - **`list_docs` filters** — `prefix` selects a family by id start, `contains`
    filters by substring; a 300-document store becomes navigable.
- **Engine-standard sheet JSON.** `doc_export op=sheet meta=standard` writes the
  industry-standard hash sprite-JSON sidecar (`frames` keyed by name with
  `frame`/`sourceSize`/`duration`, `meta.frameTags`) that game engines' existing
  sheet importers already parse. The richer native meta (pivots, collision
  boxes, palette) stays the default.

- **`doc_cast_shadow`** — a projected ground shadow (full profile). Unlike
  `doc_drop_shadow` (a flat offset copy), it flattens the caster silhouette onto
  its contact row and shears it away from the light (`az` — the vector
  `doc_form_audit` infers), stretched by `length` and foreshortened by `squash`,
  so a tall shape throws a long shadow anchored at its feet. With a
  `receiver_layer` the shadow is painted onto that layer and clipped to its
  opaque pixels (it only lands on the ground); otherwise it is drawn behind the
  caster. Completes the lighting story: form → rim → cast.

- **`doc_form_audit`** — per-form shading audit (full profile). For each
  connected opaque form it infers the light direction from a least-squares fit
  of perceptual lightness (`light_azimuth_deg`, `plane_fit_r2`) and flags
  **pillow-shading** — brightness that hugs the silhouette centre instead of a
  light direction (`pillow_corr`) — plus whether the forms share one light
  (`dominant_light_azimuth_deg` / `light_spread_deg`). Deterministic, reuses the
  existing component + interior-distance + OKLab machinery. The see-and-critique
  eye for the beginner tell the scalar reports structurally couldn't surface.
  Also wired into `doc_critique`, which now reports per-form pillow-shading and a
  mixed-light-direction check in its scorecard (replacing the old whole-image
  radial pillow guess) — so the see-and-fix loop catches it without a separate call.

### Changed

- **Own identity.** Retired the "Aseprite-as-API" framing across every
  user-facing surface (README, CLI help, the MCP instructions blurb agents read,
  crate docs). atelier is positioned by what it *is* — the see-and-correct loop
  (`doc_look` + critique) and authored-by-construction determinism — not by
  comparison to another editor.
- **Continuous-tone FX re-snap to the locked palette by default.** `blur`,
  `drop_shadow`, `bevel`, `form` and `shade` used to leave blended tone
  off-palette, blowing an N-colour palette into hundreds (`doc_glow` and
  `gradient` already re-snapped; these did not). They now snap on the
  `doc_draw` / `doc_fx` / `doc_batch` path by default — opt out per op with
  `snap:false` — so effect output stays crisp on-palette pixel art.

### Fixed

- **Stroke pose double-quantize.** The coverage stroke core is sub-pixel, but
  `Document::stroke` took integer points, so `f32` curve / IK-pose samples were
  rounded to whole pixels and re-floated — collapsing sub-pixel motion. A new
  sub-pixel `stroke_f` feeds the core directly (`stroke` is now its integer
  wrapper), `doc_figure` / `doc_walk` posing is carried in `f32` end to end, and
  batch stroke points parse as `f32`. Walk cycles and IK-solved limbs move
  smoothly frame to frame instead of stepping.

## [1.2.0] — 2026-06-28

The drawing-quality + consolidation release. The engine that was choppy and
palette-blowing now draws connected, tapered, perceptually-shaded, palette-true
art — and the agent can finally *see* its own error to repair it. The tool surface
was also restructured into op-dispatch tools (`doc_draw` / `doc_fx` / `doc_region`
/ …) and the codebase split into a 4-crate workspace. 65 tools — advertised as a
~28-tool **core** profile by default (`ATELIER_PROFILE=full` for the rest), 198 tests.

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
- **Shape tools reframed as blocking** — the shape-op descriptions now state a
  stamped shape is a *base*, never a finished sprite, and mandate the volume +
  pixel-detail + polish pass (the fix for clumsy stamped art); ≥48px canvases for
  detailed characters.
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
