# MCP tool reference

The complete tool surface, as advertised to MCP clients. Everything is drawn at
native resolution and scaled up nearest-neighbour on export, so the pixel grid
stays crisp.

## The model

- A **document** is the unit (think one `.ase` file): a canvas of ordered
  **layers** (opacity / visibility / blend, source-over composite) over a
  timeline of **frames** (each with a duration), plus animation **tags** (named
  frame ranges). A **cel** is one layer×frame image. Documents are stored under
  `~/.atelier/documents/<id>/` (override the root with `ATELIER_HOME`) and
  addressed by `id`. No projects, no baked-in art style — the agent draws
  everything.
- The loop: `doc_create` → paint (`doc_*`) → `doc_render` (flatten a frame to a
  PNG you can SEE) → inspect → fix → `doc_export_sheet` / `doc_export_gif`.

## Library

- `doc_create` — new layered/animated document (name, width, height) → `id`.
- `list_docs` — all documents (id, name, size, frame/layer counts).
- `doc_info` — a document's full structure (layers, frames, cels, tags).
- `delete_doc` — remove a document and its files.
- `export_all` — export every document as a spritesheet PNG (+ JSON meta) into a
  flat target dir for a game's `assets/`.

## Structure & timeline

- `doc_add_layer` / `doc_set_layer` — stack layers; toggle visibility / opacity /
  blend mode: `normal` · `multiply` · `screen` · `add` · `overlay` · `soft-light`
  · `hard-light` · `darken` · `lighten` · `color-dodge` · `color-burn` ·
  `difference` · `subtract` · `exclusion`. Real lighting: `multiply` for
  shadow/AO, `add`/`screen` for light/glow/bloom, `overlay`/`soft-light` to grade.
- `doc_add_frame` (optionally `copy_from` an existing frame) /
  `doc_set_frame_duration`.
- `doc_tween` — insert N cross-faded in-between frames between two poses
  (dissolve), reindexing later cels.
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

## Drawing

Coords are document pixels; color is `[r,g,b]` or `[r,g,b,a]`, alpha `0` erases.

- `doc_pencil` · `doc_line` · `doc_rect` · `doc_ellipse` (filled or clean closed
  outline; `rx==ry` ⇒ circle) · `doc_fill` (bucket) · `doc_outline` ·
  `doc_fill_cel` · `doc_clear_cel`
- `doc_polygon` — vertices `[[x,y],...]`; `fill` scanline-fills the interior,
  else draws the closed outline (organic canopies, ponds, bodies).
- `doc_polyline` — connected segments through `[[x,y],...]` (`closed` loops it
  back); square brush `size`.
- `doc_bezier` — Bézier curve through control points (2 = line, 3 = quadratic,
  4+ = cubic): smooth organic strokes — tails, vines, hair.
- `doc_gradient` — linear / radial gradient from colour `stops`, with optional
  `bayer` / `noise` ordered dithering (band-free skies, water, light falloff,
  vignettes) and a clip `region`. Replaces hand-placed dither pixels.
- `doc_scatter` — paint random `colors` across a region at a `density`, seeded &
  deterministic (organic grass, foliage, dust, stars, noise) — no hand-listing
  every speckle.
- `doc_symmetry` — mirror a cel across a vertical and/or horizontal axis (draw
  half a sprite, mirror the rest); `doc_replace_color` (recolour) · `doc_flip` ·
  `doc_shift` (`wrap` rolls edges for seamless tiles).
- `doc_stamp_image` — place an external PNG into a cel with optional `scale` /
  `rotate`, drawn OVER existing content (`opacity` + `blend`) for sub-sprite
  reuse, or `replace` the whole cel. Import bridge for AI-gen / scanned / Figma.
- `doc_text` — stamp a string with the built-in 3×5 pixel font (top-left at
  `(x,y)`, integer `size`): covers A-Z, 0-9 and `. , : ! ? - + / ( ) '` and
  space; lowercase maps to uppercase, unknown chars render as a hollow box.
  Returns the rendered `width` so you can lay out the next element — HUD mockups,
  damage numbers, lettering.
- `doc_batch` — apply many ordered ops to one cel in a single call (fast headless
  editing). Each op is `{"op":"rect|line|ellipse|polyline|polygon|bezier|pencil|
  fill|replace_color|flip|shift|outline|fill_cel|clear_cel|gradient|scatter|
  noise|adjust|blur|quantize|symmetry|drop_shadow|glow|bevel|text", ...}`; add
  per-op `opacity` / `blend_mode` to composite that op instead of overwriting.

## Effects, colour & procedural

- `doc_outline` (`aa` softens corners) · `doc_drop_shadow` (offset, blur) ·
  `doc_glow` (bloom via blur + light blend) · `doc_bevel` (raised top-left /
  shadowed bottom-right edges) — real lighting & finishing, self-contained on
  one cel.
- `doc_noise` — `cloud` (fBm) / `perlin` / `voronoi` noise mapped through colour
  stops: terrain, clouds, organic mottling.
- `doc_blur` — premultiplied box blur (soft shadows, depth-of-field, smoke).
- `doc_adjust` — shift hue / saturation / lightness over a region (tint,
  recolour, brighten).
- `doc_quantize` — snap to a palette, or derive one of N colours by median cut
  (posterise / down-palette imported art).
- `palette_ramp` — generate a hue-shifted shading ramp from a base colour (warm
  highlights, cool shadows); optionally store it as a document's palette.
- `doc_palette_swap` — recolour a whole document in one call: swap each `from[i]`
  colour to `to[i]` across every cel (exact match, all channels), updating the
  stored palette too; optional `layer` / `frame` restrict scope. The
  recolour-variant workflow — one sprite, many palettes.
- `doc_shade` — on-ramp shading from a light direction: rim pixels facing the
  light shift up the colour ramp, pixels facing away shift down (hue-shifted
  when no explicit `ramp` is given). The agent supplies form and light; the
  tool enforces ramp discipline — unlike `doc_adjust`'s flat HSL shift.
- `doc_dither` — dithering as a brush: blend two chosen colours across a
  region with `checker` / `bayer2/4/8` patterns at a `density`, optionally only
  repainting pixels that already hold those colours.
- `doc_pixel_perfect` — clean strokes to pixel-perfect form: removes the
  doubled L-corner pixels rasterized lines leave behind (the Aseprite
  convention). The agent draws the line; the tool enforces the discipline.

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
- `doc_coverage_map` — coarse occupancy/value heatmap as numbers, plus content
  bbox and centring offset: composition balance without dictating it.
- `doc_render_value` — render in analysis space: `grayscale` luma,
  `bands` (posterize to N tonal steps), `saturation` or `hue` isolation, with
  an optional numeric report (min/max/mean/contrast, per-band %). The squint
  the agent can't do.
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
  eyeballing `doc_render tile=N`.
- `doc_anim_audit` — `seam`: pixel diff of the transition the loop actually
  plays (honours tag direction; pingpong has no seam) as a seam score.
  `spacing`: per-frame silhouette-centre offsets + evenness — the animator's
  timing chart.

## Selection, regions & clipboard

The limb/keyframe-animation toolkit.

- `doc_select` — set an active pixel mask (`shape`: `rect` / `ellipse` / `color`
  / `all` / `none`), combined with the current one via `mode` (`replace` / `add`
  / `subtract` / `intersect`). While set, every painting op (fill, gradient,
  scatter, rect, ellipse, polygon, pencil, line, batch…) is confined to it —
  e.g. select a pond shape, then gradient + scatter only inside it.
- `doc_get_pixel` — read one pixel back as RGBA + `#rrggbbaa` (verify colours
  while editing blind).
- `doc_move_region` — copy a rectangle, clear the source, stamp it at `(dx,dy)`.
  Draw a limb once, nudge it per frame.
- `doc_copy_region` / `doc_cut_region` / `doc_paste` — a shared clipboard that
  works across frames **and** documents (`blend` keeps the destination under
  transparent pixels; overwrite stamps everything).
- `doc_clear_region` — erase a rectangle.
- `doc_set_palette` — lock a cohesive list of swatches on the document; stored
  and emitted in exports so a whole sprite set stays on-palette.

## Render & export

- `doc_render` — flatten a frame to a PNG preview (the agent's feedback channel).
  Options: `region` crops, `onion` ghosts the neighbour frames, `tile` repeats
  N×N to check seamlessness, `max_size` makes a cheap thumbnail.
- `doc_export_sheet` — horizontal spritesheet PNG + JSON meta (frame rects,
  durations, tags, pivots, collision boxes, palette) so any engine can slice and
  play it.
- `doc_export_gif` — animated GIF honouring per-frame durations. Pass a `tag` to
  play that animation in its direction (`forward` / `reverse` / `pingpong`);
  omit it to play the whole timeline forward.
- `doc_export_apng` — the same animation as an APNG: lossless, full alpha (unlike
  GIF's 256 colours and 1-bit alpha). Honours `tag` direction; omit it to play
  the timeline forward.
- `doc_export_tileset` — slice frame 0 into a `tile_w`×`tile_h` grid and write an
  engine-ready tileset: the PNG plus two sidecars — `<name>.tsx` (Tiled XML) and
  `<name>.json` (same fields: tilewidth / tileheight / tilecount / columns /
  image). The canvas must divide exactly by the tile size; `scale` upscales both.
- `doc_wang_tiles` — generate the deterministic 16-tile Wang/blob terrain set
  from a source doc (layer 0 = inner material, layer 1 = outer; top-left N×N of
  each sampled) into a NEW `<id>-wang` document (4N×4N, the 16 corner
  combinations in a 4×4 grid). Each set corner bit fills a quarter-disc; adjacent
  set corners connect along their shared edge.
- `export_all` — one spritesheet per document into a flat dir.
- `export_atlas` — pack **every frame of every document** into a single atlas PNG
  + master JSON (`doc`, `frame`, `rect`, `duration_ms`, `pivot`, `boxes`) so a
  whole game's sprites slice from one texture.

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
doc_render   doc_id="cat" frame=0 scale=8                  → PNG to LOOK at
doc_add_frame doc_id="cat" copy_from=0                     → frame 1 (dupe)
doc_batch    doc_id="cat" layer=0 frame=1 ops=[ …eyes… ]   → repaint as closed
doc_add_tag  doc_id="cat" name="blink" from=0 to=1 direction="pingpong"
doc_export_gif doc_id="cat" out_path="cat.gif" tag="blink" scale=8
```

> An animation needs **two or more frames**: draw frame 0, `doc_add_frame`
> (`copy_from` it), repaint the difference, then tag and export. A single-frame
> document exports a still PNG, not a moving GIF.
