# MCP tool reference

The complete tool surface. Everything is drawn at native resolution and scaled up
nearest-neighbour on export, so the pixel grid stays crisp.

> **Profiles.** By default the server advertises a **core** set of ~30 tools (the
> canonical workflows); `ATELIER_PROFILE=full` advertises all 70 below. The
> profile filters discovery only — every tool still executes (recipes/`replay`
> always work). Tools below that aren't in the core set are the *full*-only tail.

## The model

- A **document** is the unit (think one `.ase` file): a canvas of ordered
  **layers** (opacity / visibility / blend, source-over composite) over a
  timeline of **frames** (each with a duration), plus animation **tags** (named
  frame ranges). A **cel** is one layer×frame image. Documents are stored under
  `~/.atelier/documents/<id>/` (override the root with `ATELIER_HOME`) and
  addressed by `id`. No projects, no baked-in art style — the agent draws
  everything.
- The loop: `doc_create` → paint (`doc_*`) → `doc_look` (flatten a frame to a
  PNG you can SEE) → inspect → fix → `doc_export` (op=sheet|anim).

## Library

- `doc_create` — new layered/animated document (name, width, height) → `id`.
- `list_docs` — documents (id, name, size, frame/layer counts); `prefix` selects
  a family by id start (`hero-`), `contains` filters by substring.
- `doc_info` — a document's full structure (layers, frames, cels, tags).
- `delete_doc` — remove a document and its files.
- `export_all` — export every document as a spritesheet PNG (+ JSON meta) into a
  flat target dir for a game's `assets/`.

## The game layer — sets of documents

A game is not one sprite but a SET of documents that must read as one work.

- `doc_set_audit` — audit N documents (by `ids` and/or id `prefix`) as ONE game:
  per-doc palette/value/scale/pivot stats plus set cohesion — palette union size,
  unlocked docs, cross-doc near-duplicate colours (OKLab ΔE), silhouette-height
  scale outliers vs the set median, the set value range, missing pivots. Verdict
  is `cohesive` or a list of actionable warnings.
- `doc_set_palette_sync` — broadcast ONE palette across a set: lock it on every
  member and perceptually snap every cel onto it (explicit `palette` colours or
  `from_doc` to copy another document's). The fix for set-audit palette warnings.

## Structure & timeline

- `doc_layer` — layer structure in one tool. `op`: `add` (new layer on top) ·
  `set` (toggle a layer's visibility / opacity / blend) · `move` · `insert` ·
  `delete` · `rename` · `duplicate` · `merge_down`. Blend modes: `normal` ·
  `multiply` · `screen` · `add` · `overlay` · `soft-light` · `hard-light` ·
  `darken` · `lighten` · `color-dodge` · `color-burn` · `difference` · `subtract`
  · `exclusion`. Real lighting: `multiply` for shadow/AO, `add`/`screen` for
  light/glow/bloom, `overlay`/`soft-light` to grade.
- `doc_frame` — frame lifecycle + timing in one tool. `op`: `add` (append,
  optional `copy_from`) · `duration` (set a frame's ms) · `delete` (last frame
  protected) · `insert` · `duplicate` · `move`, with cels reindexed and tag
  ranges remapped. The recovery path for a bad tween or extra pose.
- `doc_dissolve` — insert N cross-faded DISSOLVE frames between two poses
  (palette-snapped, tags remapped, pivot/boxes inherited). For fades and FX
  only — never pose motion (limbs ghost); auto-checkpoints first.
- `doc_add_tag` — named frame range (`forward` / `reverse` / `pingpong`).
- `doc_set_pivot` — set a frame's anchor point `[x,y]` (feet, weapon mount); the
  engine reads it to position the sprite. Emitted (scaled) in sheet/atlas JSON.
- `doc_set_frame_boxes` — set a frame's collision boxes: a list of
  `{name, kind: body|hit|hurt, rect: [x,y,w,h]}` (`body` = collision, `hit` =
  deals damage, `hurt` = takes damage). Replaces the frame's whole set; `[]`
  clears. Pure gameplay metadata — never rasterized, emitted (scaled) in
  sheet/atlas JSON beside `pivot` so the engine drives gameplay straight off the
  sheet. Pairs with `doc_components`, which already reports tight per-blob bboxes
  if you want to size a box from the art.
- `doc_keyframe_move` — eased multi-frame region motion: take a region from one
  frame and stamp it across following frames along an interpolated offset
  (`linear` / `ease-in` / `ease-out` / `ease-in-out` cubic, `bounce`,
  `overshoot` (shoots past then settles), `elastic` (decaying oscillation)). A
  jump arc is two calls (rise ease-out, fall ease-in) — motion as structure, no
  redrawing.
- `doc_extract_to_layer` — cut a part (region rect or active selection) of a
  flat sprite onto its OWN named layer directly above, same coordinates,
  optionally across all frames — the rig step that makes per-part motion
  possible.
- `doc_keyframe_transform` — swing a part about an arbitrary JOINT pivot across
  a frame range: each frame gets the eased rotation (rotsprite-supersampled) +
  eased translation, source region cleared first, pixels snapped back to the
  locked palette. "Rotate the arm 30° about the shoulder over frames 1–4" in
  one call — the replacement for blind per-frame limb repainting.

## Drawing

Coords are document pixels; color is `[r,g,b]` or `[r,g,b,a]`, alpha `0` erases.

- `doc_draw` — apply ONE drawing op to a cel; `doc_batch` runs many in one call.
  Both share the same op vocabulary (`op` + the op's own params), all honouring an
  active selection and accepting `opacity` / `blend_mode`:
  - **pencil** `{points,color,size?}` · **line** `{x0,y0,x1,y1,color,size?}` ·
    **rect** `{x0,y0,x1,y1,color,fill?,size?}` · **ellipse** `{cx,cy,rx,ry,color,fill?}`
    (filled or clean outline; `rx==ry` ⇒ circle) · **fill** `{x,y,color,tolerance?}`
    (bucket) · **fill_cel** `{color}`.
  - **polygon** `{points,color,fill?}` — `fill` scanline-fills the interior
    (canopies, ponds, bodies); **polyline** `{points,color,size?,closed?}` —
    connected segments through `[[x,y],...]`.
  - **stroke** `{points,color,width?,aa?}` — a CLEAN tapered stroke: a union of
    round-capped capsules, **connected** by construction (no gaps), **tapered**
    (per-vertex `width`, or `[[x,y,w],...]`; `w=0` ⇒ a 1px point) and
    **anti-aliased**, snapped on-palette. The fix for choppy curves and action
    arcs; a 2-point call is a tapered capsule LIMB that stays attached when it
    shares an endpoint with another.
  - **gradient** `{stops,kind?,x0,y0,x1,y1,dither?,…}` — linear/radial from colour
    stops with optional `bayer`/`noise` dithering (band-free skies, water).
  - **scatter** `{colors,x0,y0,x1,y1,density?,seed?,size?}` — seeded speckles
    (grass, foliage, dust, stars); **noise** `{stops,x0,y0,x1,y1,kind?,…}` —
    `cloud`(fBm)/`perlin`/`voronoi` through colour stops (terrain, clouds).
  - **text** `{x,y,text,color,size?}` — the built-in 3×5 pixel font (A-Z, 0-9 and
    common punctuation; lowercase maps to upper; returns the rendered `width`).
- `doc_clear_cel` — wipe a cel (kept as its own tool).
- `doc_figure` — build a whole CONNECTED humanoid from named JOINT coordinates
  (`{"head":[x,y],"shoulder_l":[x,y],…,"foot_r":[x,y]}`): each bone is fleshed as
  a tapered `doc_draw` op=stroke capsule sharing its endpoints, so the body is one
  connected silhouette by construction — no detached limbs, no rect stacks. You
  reason in joint space (which you do well) instead of placing every silhouette
  vertex (which you don't). Re-pose across frames by calling again with new
  joints — the base for non-wobbly animation. `limb_w`/`torso_w`/`head_r` size
  it to the sprite.
- `doc_pose_cycle` — GENERATE a full animation cycle for a named GAIT from one
  standing pose (the same 13 joints) — the moveset generator. `gait`: `idle`
  (breathing bob) · `run` (airborne stride, pumping arms, forward lean) · `jump`
  (crouch → rise+tuck → fall → landing absorb) · `attack` (lead-arm sweep with a
  lunge) · `hurt` (recoil and recover). Knees/elbows solved by 2-bone IK, every
  frame the connected-capsule figure; amplitudes scale from the figure's own leg
  length × `intensity`, so presets fit any sprite size. Frames tagged with the
  gait — one call per gait builds a whole character moveset from the SAME pose.
- `doc_walk` — GENERATE a side-view walk cycle from a base standing pose (the
  same 13 joints): feet stride along a gait path (one planted, one swinging, half
  a cycle apart), knees/elbows solved by 2-bone IK, arms counter-swing the legs,
  the body bobs — each frame drawn as the connected-capsule figure, range tagged
  `walk`. Generated from joints, not hand-painted, so limbs never wobble or
  detach. Tune `frames`/`stride`/`lift`/`bob`/`arm_swing`; export with
  `doc_export op=anim tag=walk`.
- `doc_paint_grid` — paint a whole region DECLARATIVELY from a character grid:
  `legend` maps single chars to `[r,g,b(,a)]` colours or integer palette
  indices (palette-true by construction), `rows` are pixel-row strings
  (`.`/` ` leave the pixel untouched). The inverse of `doc_dump_region`;
  eliminates absolute-coordinate mistakes for detailed shapes.
- `doc_stamp_image` — place an external PNG into a cel with optional `scale` /
  `rotate`, drawn OVER existing content (`opacity` + `blend`) for sub-sprite
  reuse, or `replace` the whole cel. Import bridge for AI-gen / scanned / Figma.
- `doc_batch` — apply many ordered ops to one cel in a single call (the multi-op
  form of `doc_draw`; fast headless editing). Each op is `{"op":"rect|line|
  ellipse|polyline|polygon|stroke|pencil|fill|fill_cel|clear_cel|gradient|scatter|
  noise|text|replace_color|flip|shift|outline|adjust|blur|quantize|symmetry|
  drop_shadow|glow|bevel", ...}`; add per-op `opacity` / `blend_mode` to composite
  that op instead of overwriting.

## Effects, colour & procedural

- `doc_fx` — apply ONE op that REWORKS existing pixels (the complement of
  `doc_draw`, which adds marks; `doc_batch` runs many). `op` + flattened params,
  all honouring an active selection and accepting `opacity` / `blend_mode`:
  - **effects** — `blur` `{radius,region?}` (box blur: shadows, depth-of-field) ·
    `outline` `{color,aa?}` (flat keyline) · `drop_shadow` `{color,dx?,dy?,blur?}` ·
    `bevel` `{light,dark,depth?}` (raised top-left / shadowed bottom-right) ·
    `shade` `{light_dir?,steps?,mode?,ramp?,region?}` (on-ramp directional shading
    — enforces ramp discipline, unlike a flat HSL shift) · `form`
    `{form,light_dir?,ramp?,strength?,region?}` (sphere/cylinder/auto volume) ·
    `dither` `{color_a,color_b,pattern?,density?,region?,only_existing?}`
    (checker/bayer blend of two colours) · `pixel_perfect` `{region?,color?}`
    (drops the doubled L-corner pixels rasterized lines leave).
  - **transform** — `flip` `{horizontal?}` · `shift` `{dx?,dy?,wrap?}` (`wrap`
    rolls edges for seamless tiles) · `symmetry`
    `{vertical?,horizontal?,keep_left?,keep_top?}`.
  - **colour** — `quantize` `{colors,max_colors?}` (snap to a palette or median-cut
    to N) · `replace_color` `{from,to,tolerance?}` · `adjust` `{hue?,sat?,lum?,region?}`.
- `doc_glow` — bloom via blur + light blend; with a palette locked it re-snaps the
  bloom back ON-palette by default (`snap=false` keeps it soft). Its own tool
  because the on-palette `snap` isn't a `doc_fx` / batch op.
- `doc_rim_light` — paint a RIM/edge light on silhouette edges FACING the light
  (`az`: 0=right, 90=down, 180=left, 270=up); `dark=true` lights the away-facing
  edge (core/contact shadow). Topological — survives small canvases where a
  Fresnel rim washes out.
- `doc_cast_shadow` — a projected GROUND shadow (not a flat offset copy like
  `drop_shadow`): the caster silhouette flattened onto its contact row and
  sheared AWAY from the light (`az`), stretched by `length` and foreshortened by
  `squash`. With `receiver_layer` it lands on that layer clipped to its opaque
  pixels (the ground); else it sits behind the caster. Pairs with the light
  vector `doc_form_audit` infers.
- `doc_palette_swap` — recolour a whole document in one call: swap each `from[i]`
  colour to `to[i]` across every cel (exact match, all channels), updating the
  stored palette too; optional `layer` / `frame` restrict scope. One sprite, many
  palettes.

All painting / effect / procedural ops above honour an active `doc_select` and a
layer's blend mode. `doc_batch` validates op fields strictly — a misspelled or
misplaced key fails fast with the expected key list instead of silently
drawing garbage.

## Analysis & critique

Read the canvas as *data* — the agent's other eye.

- `doc_dump_region` — a region as a text grid (one char per pixel + colour
  legend, or hex tokens): exact pixel verification instead of squinting at a
  PNG.
- `doc_silhouette` — the alpha silhouette as a `#`/`.` grid + bbox + fill
  ratio: the classic "does the pose read?" squint test, as data.
- `doc_components` — connected-component report (bbox, centroid, area,
  dominant colour per blob, stray 1–2px specks listed separately): catches
  floating pixels and detached limbs a thumbnail hides.
- `doc_form_audit` — per-form shading audit: infers each form's light direction
  (lightness-plane fit) and flags pillow-shading (brightness hugging the centre
  instead of a light) plus whether the forms agree on one light. Sees the #1
  beginner failure the scalar reports can't.
- `doc_coverage_map` — coarse occupancy/value heatmap as numbers, plus content
  bbox and centring offset: composition balance without dictating it.
- `doc_contrast_check` — WCAG contrast ratios: a region vs its surround, all
  palette pairs, or a `one-bit` black/white threshold render — readability as
  numbers.
- `doc_palette_report` — every distinct colour with pixel counts, % coverage,
  in-locked-palette flags, and near-duplicate pairs: the real colour budget,
  including the stray AA tints.
- `doc_ramp_validate` — critique a shading ramp: monotonic value? even
  spacing? hue-shift direction and size per step? Warnings name the bad steps.
- `doc_frame_diff` — what actually changed between two frames: counts by kind
  (added/removed/recoloured), change bbox, optional text grid and overlay
  render. The flip-book the agent can't do.
- `doc_seam_report` — exact wrap-mismatch pixels for tile work (horizontal /
  vertical), worst offenders with deltas, optional highlight render — replaces
  eyeballing `doc_look tile=N`.
- `doc_anim_audit` — `seam`: pixel diff of the transition the loop actually
  plays (honours tag direction; pingpong has no seam) as a seam score, plus the
  change_bbox naming WHERE it pops. `spacing`: per-frame opaque-mass-centroid
  offsets + evenness (pass `region` to isolate one limb over a static body).
  `arc`: trajectory shape + volume constancy. `timing`: per-frame durations
  with a uniform-timing flag.

## Selection, regions & clipboard

The limb/keyframe-animation toolkit.

- `doc_select` — set an active pixel mask (`shape`: `rect` / `ellipse` / `color`
  / `all` / `none`), combined with the current one via `mode` (`replace` / `add`
  / `subtract` / `intersect`). While set, every painting op (fill, gradient,
  scatter, rect, ellipse, polygon, pencil, line, batch…) is confined to it —
  e.g. select a pond shape, then gradient + scatter only inside it.
- `doc_region` — region + clipboard ops on a cel. `op`: `copy` · `cut` (copy +
  clear) · `paste` (clipboard at `x,y`; `blend` keeps the destination under
  transparent pixels, overwrite stamps everything) · `move` (shift the rect
  `[x0,y0,x1,y1]` by `dx,dy` in place — draw a limb once, nudge it per frame) ·
  `clear` (erase the rect). The clipboard works across frames **and** documents.
- `doc_set_palette` — lock a cohesive list of swatches on the document; stored
  and emitted in exports so a whole sprite set stays on-palette.

## Render & export

> Looking at a frame is `doc_look` (under **See & measure** below — it's the one
> SEE call, inline). This section is the file-writing export tools.

- `doc_export` — write a document to a file; `op` + shared `out_path`/`scale`:
  - **sheet** — horizontal spritesheet PNG + JSON meta (frame rects, durations,
    tags, pivots, collision boxes, palette) so any engine can slice and play it.
    `meta=standard` writes the industry-standard hash sprite-JSON instead —
    `frames` keyed by name with `frame`/`sourceSize`/`duration` and
    `meta.frameTags` — the shape engines' existing sheet importers already
    parse. That shape has no slot for pivots/boxes; use the native meta when the
    engine should read those.
  - **anim** `{format?,tag?}` — animation as `format=gif` (256 colours + 1-bit
    alpha, smaller) or `apng` (lossless, full alpha), honouring per-frame
    durations. A `tag` plays that animation in its direction
    (`forward`/`reverse`/`pingpong`); omit to play the whole timeline. All frames
    snap to ONE shared palette before encoding, so colours don't shimmer.
  - **tileset** `{tile_w,tile_h}` — slice frame 0 into a grid → PNG + `<name>.tsx`
    (Tiled XML) + `<name>.json`. Canvas must divide exactly by the tile size.
- `doc_wang_tiles` — generate the deterministic 16-tile Wang/blob terrain set
  from a source doc (layer 0 = inner material, layer 1 = outer; top-left N×N of
  each sampled) into a NEW `<id>-wang` document (4N×4N, the 16 corner
  combinations in a 4×4 grid). Each set corner bit fills a quarter-disc; adjacent
  set corners connect along their shared edge.
- `export_all` — one spritesheet per document into a flat dir.
- `export_atlas` — pack **every frame of every document** into a single atlas PNG
  + master JSON (`doc`, `frame`, `rect`, `duration_ms`, `pivot`, `boxes`) so a
  whole game's sprites slice from one texture.

## World-class craft (the art-quality pass)

The tools from the art-quality pass. The theme: let the near-blind agent *see*
and *measure*, edit *structurally* and *non-destructively*, and reach
*perceptual* colour and master finish.

**See & measure (the agent's eye).**

- `doc_look` — the primary (and only) SEE call: a frame as an **inline PNG** (no
  separate file read) plus measured stats, in one turn. `mode`: `render` ·
  `value` · `bands` · `sat` · `hue` · `notan` (3-value squint). `grid`/`coords`
  burn a pixel ruler into the upscale; `onion` ghosts neighbours; `region` crops;
  `max_size` makes a thumbnail; `tile` repeats the result N×N to check
  seamlessness; `out_path` also writes the PNG to a file. Stats report value
  min/max/mean/contrast and shadow/mid/light mass % — plus per-band coverage in
  `bands`/`notan` modes.
- `doc_select_render` — the active selection as a quick-mask overlay (selected
  art shown, the rest dimmed + magenta-tinted) so you never paint through an
  unseen mask. Inline PNG + selected-pixel count/bbox.
- `doc_contact_sheet` — every frame in one labelled inline grid (the flip-test);
  `onion=true` ghosts each cell's previous frame under it — per-pair onion skin.
- `doc_critique` — the art-director scorecard: orphan specks, un-AA'd jaggies,
  low contrast, pillow-shading, value-soup massing and off-palette drift, with
  worst-offending cells. Conservative verdicts (ok/warn/info).
- `doc_translucency_report` — glass/glow alpha as data: opaque/partial/
  transparent counts, mean alpha, partial-alpha band histogram + bbox.
- `doc_anim_audit mode="arc"` — trajectory arc-residual (straight slide vs real
  arc) + volume constancy, beside the existing `seam`/`spacing`.

**Edit without fear (structure & non-destructive).**

- `doc_checkpoint` — `save` · `list` · `restore` · `diff` · `prune`: snapshot a
  document before a risky op and roll back, or diff regression deltas
  (pixel/colour/contrast change, added/removed/recoloured). Undo for a
  destructive editor.
- `doc_transform_cel` — affine-transform a cel/region **in place**: rotate,
  scale, skew. `method` `rotsprite` (super-sampled, cluster-preserving) or
  `nearest`; `preserve_volume` (squash-and-stretch), `snap_palette`,
  `clear_source` (move vs overlay).
- `doc_select_wand` — contiguous magic-wand → the active selection (perceptual
  OKLab tolerance, 4/8-connectivity, `replace`/`add`/`subtract`/`intersect`).

**Perceptual colour (OKLab/OKLCh).**

- `doc_palette` — the OKLCh palette generator: `scheme=mono` for a single
  perceptually-even shading ramp (equal-lightness steps, hue shift, `sat_curve`
  flat/arc/sat-in-shadow, `anchor_midtone`, evenness validation) or
  `complementary/triadic/analogous/split/tetradic` for a cohesive multi-hue set
  sharing lightness poles. `count` per ramp; `set_doc` locks it. (replaces `palette_ramp` / `doc_make_perceptual_ramp` / `doc_harmony_palette`)
- `doc_snap_palette` — snap a cel/document to its locked palette by perceptual
  nearest colour, killing off-palette drift from blends/dithers/FX. `alpha`
  governs FX bloom / AA fringe: `preserve` (default, RGB-only) · `opaque`
  (binarise alpha at `cutoff`, default 128 — collapse a soft bloom into crisp
  on-palette pixels) · `flatten` (composite over `bg`, then snap fully opaque).

**Master finish & form.**

- `doc_relight` — multi-light form shading (key/fill/rim): silhouette → height →
  normals → Lambert + Fresnel rim. Azimuth/elevation lights, ambient, `bulge`;
  multiplies the base (light colour tints) or snaps to a `ramp`.
- `doc_smooth_edges` — selout: an opaque mid-value pixel into each outer
  staircase notch so diagonals read smooth; `keep_square` preserves deliberate
  right angles; `ramp` keeps the AA on-palette.
- `doc_dither_ramp` — graduated multi-tone dithering across a whole ramp along
  `h`/`v`/`radial`, ordered or `ign` blue-noise — master gradient shading.
- `doc_outline_selective` — form-following contour coloured from the fill it
  borders (vs a flat black keyline).
- `doc_material` — procedural material recipes: metal/wood/stone/water/cloth/
  skin/glass, from one base colour, deterministic in `seed`, on the opaque pixels.
- `doc_box` — a 3-face shaded isometric cuboid (hard-surface form `form` can't
  make).
- `doc_perspective_guide` — a faint, deletable guide layer (thirds/grid/iso/vp).
- `doc_panel` — a HUD/UI panel (fill + border + bevel).
- `doc_burst` — radial FX frames (ring/disc/rays) tagged `burst`.
- `doc_ref op=import` — external image (AI-gen/photo/scan) → clean pixel art:
  TRUE area-average downscale (aspect-derived height when `target_h` omitted),
  optional corner-flood `remove_bg` BEFORE palette extraction, frequency-
  weighted median-cut palette with `pin`ned colours, optional Floyd–Steinberg
  (defaults off ≤64px where it reads as speckle), inline preview.

## Reference loop (recreate from a sample)

The closed loop that turns "does my sprite match the character?" from memory
recall into a measured, same-turn signal:

- `doc_ref op=set` — attach the ORIGINAL image to a document (copied into
  the doc dir, persists with it); returns aspect-true canvas-fit suggestions.
  Omit `path` to clear.
- `doc_ref_analyze` — VIEW the reference inline and decompose it into drawing
  scaffolding: background coverage, a frequency-weighted subject palette to
  lock with `doc_set_palette`, and the subject's silhouette as a text grid at
  target size.
- `doc_ref_compare` — SCORE a frame against the reference after every pass:
  inline side-by-side (or `mode="overlay"` ghost), silhouette IoU (≥0.80 =
  shape reads), per-cell OKLab ΔE with the worst cells named as canvas rects,
  and reference colours missing from the palette.
- `doc_diff_map` — PER-PIXEL signed error map vs the reference (the see-and-repair
  eye `ref_compare`'s aggregate ΔE can't be): a HEAT png (red = too light, blue =
  too dark, green = wrong colour; brightness = ΔE) plus the `top` worst individual
  pixels, each with x,y, ΔE and a fix direction (lighten/darken + saturate/
  desaturate + shift hue). Fix the named pixels, re-run — converges the last 5%.
- `doc_critique_vision` — the AI eye for FREE-FORM art (no reference). Renders the
  frame and asks the MCP HOST to run its own vision model over it (atelier ships
  no weights, makes no network call, holds no keys — the host samples). Returns a
  structured critique: reads-as, silhouette/proportion, value/colour, top-3 fixes;
  `focus` weights one axis. Requires a host advertising the `sampling` capability
  — errors clearly if absent.

## Beyond the tools

The server exposes more than the tool list — the MCP standard surfaces, plus a
recorder and a web view.

- **MCP resources** — every document is browsable as a resource:
  `atelier://doc/<id>` is its structure JSON, `atelier://doc/<id>/render` is
  frame 0 flattened to a PNG (scale 4). Clients can list and read them without a
  tool call; unknown URIs and missing documents return `resource_not_found`.
- **MCP prompts** — packaged workflows that fill in your subject and hand the
  agent the right loop: `pixel-sprite` (`subject`, optional `size`,
  optional `palette_hint`),
  `walk-cycle` (`character`, optional `frames`), `seamless-tile` (`material`,
  optional `size`). Each names the tools it drives.
- **Session recorder** — `atelier --record <recipe.json>` (or
  `ATELIER_RECORD=<path>`) writes every tool call of a live session into a
  replayable recipe — a real session becomes a deterministic `atelier replay`
  fixture. Works with stdio and `--http`.
- **Live gallery** — the `--http` server also serves a read-only web view at
  `/gallery`: a self-contained page that polls `/gallery/docs` and renders each
  document live via `/gallery/<id>/render.png?frame=&scale=` (scale clamped
  1..=16). Open it in a browser to watch art appear as the agent draws.

## Example: a blinking sprite

The whole loop, as an agent would drive it over MCP:

```
doc_create   name="cat" width=32 height=32                 → id "cat"
doc_batch    doc_id="cat" layer=0 frame=0 ops=[ …shapes… ] → paint the body
doc_look     doc_id="cat" frame=0 scale=8                  → PNG to LOOK at
doc_frame    op=add doc_id="cat" copy_from=0                     → frame 1 (dupe)
doc_batch    doc_id="cat" layer=0 frame=1 ops=[ …eyes… ]   → repaint as closed
doc_add_tag  doc_id="cat" name="blink" from=0 to=1 direction="pingpong"
doc_export op=anim doc_id="cat" out_path="cat.gif" tag="blink" scale=8
```

> An animation needs **two or more frames**: draw frame 0, `doc_frame op=add`
> (`copy_from` it), repaint the difference, then tag and export. A single-frame
> document exports a still PNG, not a moving GIF.
