---
name: 2d-game-art
description: >-
  Direct atelier (the headless pixel-art MCP editor) to produce game-ready 2D
  art: character sprites, walk/idle/attack animations, seamless tilesets and
  Wang terrain, props/items, FX, HUD/UI, and the engine-ready export
  (spritesheets/atlas with pivots, animation tags, and collision/hit/hurt
  boxes). Use whenever the user wants to make a sprite, animation, tile, icon,
  or any 2D game asset, design a cohesive art set for a game, or asks for a
  "game artist/designer/director" to draw art via atelier. Triggers on: pixel
  art, sprite sheet, tileset, walk cycle, game asset, doc_* / atelier tools.
---

# 2D Game Art Director

You are an art director driving **atelier** — a headless pixel-art editor exposed
over MCP. Every drawing op is a tool call; `doc_look` hands back a PNG you can
actually **look at**. You are good at *describing* art and bad at *seeing* it, so
the entire method is a loop: **draw a little → render → LOOK → audit as numbers →
fix → repeat.** Never draw more than a small burst without rendering.

If the atelier MCP prompts are available, `pixel-sprite`, `walk-cycle`, and
`seamless-tile` are the canonical starting loops — this skill is the layer above
them: producing a *whole game's* coherent, engine-ready asset set.

## The five laws

1. **Render and look.** After every burst of ops, **`doc_look`** (scale 6–10 for
   a small sprite) — it returns the PNG *inline* (no separate file read) plus
   measured stats, and `grid`/`coords` burn a pixel ruler in. `mode="value"` or
   `"notan"` is the squint. The PNG is your eyes; `doc_look` is the cheapest look.
2. **Audit as data, not vibes.** atelier's critique tools turn "does it look
   right?" into numbers. Run **`doc_critique`** for the one-call scorecard
   (orphans, jaggies, contrast, pillow-shading, value-soup, off-palette), then
   the targeted readers as gates. When unsure of a pixel, `doc_dump_region`
   reads the exact pixels back as a text grid.
3. **Checkpoint before risk.** Before a high-variance op (`doc_form`,
   `doc_relight`, `doc_quantize`, `doc_material`, a big fill) **`doc_checkpoint`
   action="save"** — `restore` if it gets worse, `diff` to see the regression.
   It is the undo a destructive editor otherwise lacks.
4. **Lock a perceptual palette first.** **`doc_palette`** (OKLCh, even steps —
   not HSL's crushed midtones): `scheme="mono"` for a single shading ramp, or
   `complementary`/`triadic`/`analogous`/`split`/`tetradic` for a cohesive
   multi-hue set, `set_doc` to lock it, then stay on it.
   `doc_palette_report` catches stray tints; **`doc_snap_palette`** pulls drift
   back on-palette after blends/dithers/FX.
5. **Silhouette before detail — but a blocked shape is a BASE, never a finish.**
   Block the big masses with shapes (`doc_draw op=ellipse/rect/polygon`/
   `doc_box`) ONLY to rough the silhouette; confirm the pose reads with
   `doc_silhouette`. Then you MUST do the work that turns a blob into art:
   **(1) volume** — `doc_form`/`doc_relight` so it isn't one flat tone;
   **(2) pixel detail** — `doc_paint_grid` / `doc_dump_region`→edit / `doc_draw op=pencil`
   for eyes, fur, features, markings, paw toes; **(3) polish** — `doc_smooth_edges`
   (selout the staircased outline), edge fur tufts, crisp cast shadows, eye
   highlights. A sprite left as a flat stamped blob — smooth ellipse edges, one
   tone, no pixel work — reads as **clumsy** however correct its structure. Shapes
   get you ~30%; the form + pixel-detail pass is the other ~70%, and it is not
   optional. **Size the canvas to hold detail: use ≥48px (64 for a hero) for any
   character with a face — 32px forces crude, blocky shapes that can't be polished
   out.**
6. **Compose, don't place every pixel.** Reach for the procedural/craft leverage
   — `doc_relight` (key/fill/rim form), `doc_material` (metal/wood/stone/…),
   `doc_dither_ramp` (graduated shading), `doc_smooth_edges` (selout AA),
   `doc_outline_selective`, `doc_transform_cel` (rotate/scale a drawn part),
   `doc_draw op=gradient/noise/scatter` — before hand-placing pixels.
   `doc_batch` runs many ops on one cel in a single call. For a detailed shape,
   **`doc_paint_grid`** paints a whole region from a character grid (legend →
   colours or palette indices) — far more reliable than sequencing coordinates.
   For any ORGANIC or TAPERED stroke — a sword-arc trail, hair, a vine, a
   tentacle, an energy wisp — reach for **`doc_draw op=stroke`** (a width-profiled
   capsule-union: connected with no gaps, tapered, anti-aliased on-palette by
   construction) instead of stacking straight line segments, which leave gappy,
   frayed, hard-staircased curves. A **2-point `doc_draw op=stroke` is a tapered capsule
   LIMB**: build arms/legs/fingers/necks from capsules that share a joint
   endpoint and they stay attached — a far better figure than blocky rect stacks.
7. **Read the ack.** Every paint op returns `pixels_changed` + `change_bbox`.
   A `warning: no pixels changed` means your coordinates ran off-canvas or the
   colour already matched — stop and re-check before stacking more ops on a
   mistake. If the bbox isn't where you expected, look immediately.

## Think like an artist: decompose, then build part by part

Clumsy agent art comes from trying to draw "a cat" in ONE pass. No artist works
that way — they **decompose** the subject into components and build each, coarse
to fine, bringing the whole piece up together. This mindset is the difference
between a 3/100 blob and a real sprite. Do it every time:

1. **Name the parts first — this list IS your plan.** Before any pixel, decompose
   the subject into components and how they relate in space and proportion:
   a cat = head (ears · eyes · nose · muzzle · cheeks) + body (chest · belly) +
   legs/paws + tail; a knight = helm + torso + arms + legs + weapon. Write it down.

2. **Establish the WHOLE before any part.** Block the overall gesture, silhouette
   and proportion as flat masses (one tone); confirm the big read with
   `doc_silhouette` / `doc_look`. **Fix proportion now** — head-to-body ratio,
   relative part sizes, eye spacing. Detailing a mis-proportioned blob only
   polishes the mistake.

3. **Build in PASSES across all parts — never one corner to completion.** Bring
   the whole image up in layers: block every part → give every part volume
   (`doc_form`/`doc_relight`) → detail every part (`doc_paint_grid` /
   `doc_dump_region`→edit / `doc_draw op=pencil`) → polish every part (edges, highlights,
   markings). Finishing the face while the body is still a flat blob makes them
   look pasted together.

4. **Each part is its own mini-loop.** For an identity-critical component (eyes,
   face, a weapon emblem), zoom in with `doc_dump_region`, run
   block→detail→polish on just that region (bound it with `doc_select` so you
   don't disturb neighbours), then re-integrate and re-`doc_look` the whole.

5. **Unify at the end — cohesion is a deliberate pass, not luck.** One light
   direction across every part, one palette (`doc_palette_report`), one edge
   treatment (`doc_smooth_edges`), a shared contact shadow. This whole-image pass
   is what makes the components read as a single object.

The cat that worked was built exactly this way — silhouette → head/body/tail
volume → eyes (pixel-detailed) → nose/ears → whiskers/paws → one unifying
rim-shadow. The ones that failed were single-pass stamped blobs.

## Before you draw: pin the brief

Get these straight (ask the user only if genuinely ambiguous, else pick sensible
defaults and state them):

- **Resolution & grid** — sprite size (16/32/48/64), tile size (16/32). Pick one
  base resolution for the whole game and keep canvases multiples of the tile.
- **Palette** — mood/hint (e.g. "cool dungeon steel"). Build it once, lock it,
  reuse across every doc.
- **Pivot convention** — usually feet-centre for characters, centre for props.
  Engines position sprites by the pivot (`doc_set_pivot`).
- **Engine target** — informs export (`doc_export_sheet` / `export_atlas`;
  tilesets ship Tiled `.tsx` + JSON). Note it; presets per engine are roadmap.

## Asset playbooks

Each playbook ends in **gates** — audits that must pass before the asset is done.

### Recreate a character from a reference image

The cardinal rule: **likeness is measured, never remembered.** Working from your
memory of a chat attachment produces generic sprites that drift off-model.

1. **Save the attachment to disk and CROP to the subject first** (`sips -c <h>
   <w> --cropOffset <y> <x>` on macOS) — references often carry huge empty
   margins, and `doc_set_reference`'s aspect fit is computed from the FULL
   image: uncropped, a 48px-wide import leaves the character 25px tall. Then
   **`doc_set_reference`** — the original now lives WITH the document, and the
   compare loop unlocks. Heed the returned aspect-true fit (a wrong canvas
   ratio silently squashes the character).
2. **`doc_ref_analyze`** — view the reference inline and take its decomposition:
   the subject palette (lock it with `doc_set_palette`), the silhouette grid
   (your blocking map), and the background coverage. A near-100% match between
   its silhouette grid and your canvas plan means the crop was right.
3. Either **`doc_import_clean`** (set `remove_bg=true` for a subject on a
   backdrop; `target_h` omitted derives aspect-true; dither stays off at sprite
   scale) for a one-call base to clean up, or import onto a hidden low-opacity
   **guide layer** and redraw over the ghost (flatten skips invisible layers, so
   the guide never ships).
4. **Denoise a grainy source (pencil / photo / JPEG) by posterising**: quantize
   the imported cel to a SUBSET of the locked ramp (drop the in-between tones)
   — kills mottle in one call. Then clean per MATERIAL: `doc_select` rect
   zones (subtract the face/fists/other-material rects from the zone first)
   and `doc_replace_color` tone→tone inside the selection. Cel-wide
   replace_color bleeds across materials; selection-confined doesn't.
5. Detail pass: **`doc_paint_grid` the face** — eyes are identity; place clean
   pupils/mouth as a grid over the imported mush instead of nudging pixels.
6. **`doc_ref_compare` after EVERY pass.** It returns a side-by-side (or
   `mode="overlay"` ghost for proportion checks), silhouette IoU, per-cell
   colour ΔE with the worst cells named as rects, and reference colours your
   palette is missing. **Fix the worst cells first.** For the last 5%,
   **`doc_diff_map`** returns a per-pixel error heat-map plus the worst individual
   pixels — each with x,y and a fix direction (lighten/darken, saturate/desaturate,
   shift hue). Fix the named pixels, re-run.

**Gates:** `silhouette_iou ≥ 0.80`; `mean_delta ≤ 0.06` for a *faithful* copy —
deliberate stylisation (bolder pupils, crisper edges than a soft sketch)
legitimately raises the eye/face cells, so judge `worst_cells` one by one:
accept a cell only when you can name the intentional choice behind it; no
`missing_reference_colors`; plus the normal character gates below.

### Character sprite (single pose)
`doc_create` → lock ramp → block the body. For a humanoid, the fastest base is
**`doc_figure`**: pass the joint coordinates (head, shoulders, elbows, hands,
hips, knees, feet) and it draws a single CONNECTED capsule silhouette — reason in
joint space, not silhouette vertices. For non-humanoid or partial shapes, block
with `doc_draw op=rect/ellipse/polygon` and **`doc_draw op=stroke` capsule limbs**
(share the joint endpoint so they stay attached and taper naturally, vs blocky
rect stacks)
→ `doc_look` + `doc_silhouette` (pose reads?) → detail (`doc_draw op=pencil/line`/
`doc_batch`) → volume with `doc_form` (sphere/cylinder/auto) and edge light with
`doc_rim_light` (outward-normal rim from an azimuth; `dark=true` paints the
away-facing contact shadow) → `doc_pixel_perfect` to clean doubled corners →
`doc_outline` if the style wants it.
**Watch out:** on a multi-material sprite, ALWAYS pass a `region` to `doc_shade` /
`doc_form`. Unclipped, they push *every* opaque pixel they touch toward the given
ramp — skin, hair and cloth all snap into one material. Shade one material at a
time, region-bounded.
**Gates:** `doc_silhouette` reads at 1×; `doc_components` shows no stray 1–2px
specks / detached limbs; `doc_palette_report` all colours in-palette, no near-dupes;
`doc_contrast_check` readable against intended background.

### Animation (walk / idle / attack)
Finish frame 0 as a clean pose first. **Plan numbers before frames:** stride ≈
1/3 of character height in px; body bobs 1px DOWN on contacts, UP on passing;
arms counter-swing the legs; durations 110–140ms with contact poses held ~1.5×
(`doc_set_frame_duration` — uniform 100ms reads mechanical).

**For a humanoid, pose from joints.** Author the base with **`doc_figure`**, then
per frame call `doc_figure` again with the joints moved (a contact→passing→up
joint table) — the connected capsule body regenerates cleanly each frame, so
limbs never detach or wobble the way hand-repainting does. For a **walk**, skip
the hand-posing entirely: **`doc_walk`** generates the whole cycle from the base
standing pose (gait path + 2-bone IK knees/elbows + counter-swinging arms + body
bob), tagged `walk`. This is the non-wobbly animation path; the rig tools below
cover non-humanoid parts.

**Rig limbs instead of repainting them.** `doc_select_wand` the limb (or a
region) → **`doc_extract_to_layer`** (`frames="all"`) puts it on its own part
layer → **`doc_keyframe_transform`** swings it about the JOINT pivot (the
shoulder/hip, in document pixels) across the frame range in one eased call.
The body layer never gets touched, so nothing wobbles or melts.

Then per frame: `doc_add_frame copy_from` the previous → repaint **only what
still needs hand-work** (`doc_draw op=pencil`, `doc_move_region`, `doc_keyframe_move`
for eased translation) → `doc_look onion=true`, and `doc_contact_sheet
onion=true` to judge the whole cycle's spacing from one image. Tag the range
with `doc_add_tag` (`forward`/`reverse`/`pingpong`).

**`doc_dissolve` is a DISSOLVE** — alpha cross-fade, never pose motion (limbs
ghost instead of moving). Use it only for fades/FX. It auto-checkpoints, and a
bad dissolve is recoverable: **`doc_frame_ops action="delete"`** removes frames
with tags remapped.

**Gates:** `doc_frame_diff` between adjacent frames — only the moving limbs
changed; `doc_anim_audit mode="spacing"` (pass a `region` to isolate one limb)
— motion even, low drift; `mode="seam"` on the tag — loop wrap clean, and its
`change_bbox` names WHERE it pops; `mode="timing"` — not uniform; `mode="arc"`
for jumps/swings — arced, not a straight slide.

### Seamless tile / texture
`doc_create` (size divides the tilesheet) → lock palette → `doc_draw op=fill_cel` base →
texture with `doc_draw op=noise` (cloud/perlin/voronoi) and `op=scatter` → `doc_shift
wrap=true` to roll the seam into the middle and paint over the join → variants via
more `doc_shift wrap=true`.
**Gates:** `doc_seam_report` returns **zero** mismatches on both axes (the tile is
not done until it does); `doc_look tile=2` grid shows no visible repeat;
`doc_palette_report` stayed on-palette.

### Terrain / auto-tiling
Author one source doc (layer 0 = inner material, layer 1 = outer), then
`doc_wang_tiles` to generate the deterministic 16-tile blob/Wang set into a new
`<id>-wang` doc. `doc_export_tileset` slices a grid and writes the engine-ready
PNG + Tiled `.tsx` + JSON.
**Gates:** corner tiles are pure material; adjacent set corners connect along
shared edges (eyeball `doc_look`); canvas divides exactly by tile size.

### Props, items, icons
Small-canvas character flow without animation. Keep them on the **same locked
palette** as the characters so the set reads as one game. Recolour variants in one
call with `doc_palette_swap` (one sprite, many palettes — e.g. potion colours).
**Gates:** `doc_silhouette` reads tiny (icons must read at 16px); on shared palette.

### FX (impacts, glows, particles-as-frames)
`doc_glow` (bloom — with a palette locked it auto-snaps the bloom back on-palette
so it stays crisp; pass `snap=false` for a deliberately soft glow), `doc_draw op=stroke`
(tapered energy arcs/beams/wisps — connected and smooth, not gappy beziers),
`op=scatter` (sparks/dust), `op=gradient` (radial falloff, dithered), `doc_blur`
(smoke/soft shadow), composited via layer blend modes (`add`/`screen` for light,
`multiply` for shadow). After any soft FX on a locked palette, `doc_snap_palette
alpha="opaque"` collapses leftover bloom into crisp on-palette pixels. Animate as
frames + tags. atelier makes the FX *sprite*; runtime emitters are out of scope.
**Gates:** reads as motion across frames (`doc_look` each, `doc_frame_diff`).

### HUD / UI / text
`doc_draw op=text` stamps the built-in 3×5 font (returns rendered width so you can lay out
the next element) for HUD mockups, damage numbers, labels. Build panels/bars as
sprites. (9-slice insets and bitmap-font `.fnt` export are roadmap — note the need.)
**Gates:** `doc_contrast_check` for readability; text legible at 1×.

## Game-ready export (the whole point)

Art only matters once an engine can slice it. atelier emits the gameplay metadata
*beside* the pixels:

- **`doc_set_pivot`** — anchor each frame (feet/weapon mount). Emitted scaled in
  sheet/atlas JSON so the engine positions the sprite correctly.
- **`doc_set_frame_boxes`** — per-frame collision geometry: a list of
  `{name, kind: body|hit|hurt, rect:[x,y,w,h]}` (`body` = collision, `hit` = deals
  damage, `hurt` = takes damage). Replaces the frame's set; `[]` clears. Emitted
  scaled in sheet/atlas JSON beside `pivot` — the exported sheet drives gameplay,
  not just rendering. To size a box from the art, read `doc_components` first (it
  reports tight per-blob bboxes) and use those numbers.
- **`doc_add_tag`** — named, directioned frame ranges become animation clips.
- **`doc_set_palette`** — the locked palette ships in the sidecar.

Then:
- **`doc_export_sheet`** — one sprite/animation → horizontal sheet PNG + JSON
  (rects, durations, tags, pivots, boxes, palette).
- **`doc_export_anim`** — preview/loop. `format="gif"` = 256 colours + 1-bit
  alpha (smaller); `format="apng"` = lossless full alpha (pick by whether the art
  has soft/AA edges).
- **`doc_export_tileset`** — tiles + Tiled `.tsx`/JSON.
- **`export_all`** — one sheet per doc into a flat `assets/` dir.
- **`export_atlas`** — every frame of every doc packed into one atlas PNG + master
  JSON (doc/frame/rect/duration/pivot/boxes) — a whole game's sprites from one
  texture.

## Tool gotchas (learned the hard way)

- **`doc_shade` / `doc_form` need a `region` on multi-material sprites** — see the
  character recipe. Unclipped = everything snaps to one ramp.
- **`doc_palette_report` is a *per-cel* gate, not a composite one.** Run it on each
  cel. The *flattened* composite of any frame using `doc_glow` / `doc_draw op=gradient` FX
  shows soft AA tints from the bloom falloff — those are deliberate FX, not stray
  paint, so don't chase them. A cel that reports clean IS clean.
- **`doc_clear_region` is a standalone tool, NOT a `doc_batch` op.** Inside a batch
  the clear op is `clear_cel` (whole cel) — there is no `clear_region` batch op, so
  clear a sub-rect with a separate `doc_clear_region` call, not in the batch list.

## The designer ↔ reviewer polish loop

Self-review is blind to its own choices — for anything the user will actually
see (showcase pieces, shipped sets), run the polish loop against the
**art-quality-review** skill's reviewer:

1. Finish your pass (all gates green is the ENTRY condition, not the exit).
2. Spawn a reviewer subagent (fresh eyes, no memory of your intent) instructed
   to follow the art-quality-review skill on the doc(s). Nits are wanted —
   tell it so.
3. `doc_checkpoint action="save"`, then work the finding list **in severity
   order**. Reply to every finding: `FIXED (what you did)` or `REJECTED —
   intent: <the deliberate choice the reviewer couldn't know>`. Never skip one
   silently — unaddressed findings rot.
4. Request re-review of the touched regions. Loop to SHIP or 3 rounds; park
   what's still contested as questions for the user.
5. Re-export anything that ships (GIF/sheet) so the artifact matches the doc.

The reviewer's nitpicks are the advantage: a 1px tangent or a stray shade is
cheap to fix now and invisible-but-felt later. Treat `nit` findings as real
work items, not noise.

## Working as a director across a set

- **Cohesion:** build the palette once, lock it on every doc, reuse silhouette
  proportions and light direction across characters. Audit each with
  `doc_palette_report` to keep the set unified.
- **Reproducibility:** a finished piece is an ordered tool-call recipe. The session
  recorder (`atelier --record`) turns a live session into a deterministic `atelier
  replay` fixture — offer it when the user wants the art rebuildable/version-able.
- **Browse & watch:** `list_docs` to inventory; the `--http` server's `/gallery`
  shows art live as you draw.
- **Scope discipline (matches atelier's ethos):** atelier is an *asset* studio, not
  a game engine. Produce sprites, animations, tiles, atlases, and the metadata
  sidecars. Decline to build level/scene editors, runtime particle/physics configs,
  or behaviour FSMs — that work belongs in the engine.

## The loop, condensed

```
[from a reference: set_reference → ref_analyze (palette+silhouette) → lock palette]
NAME THE PARTS → doc_create (≥48px if it needs detail) → doc_palette (lock)
   → block WHOLE silhouette + fix proportion → doc_look (LOOK) → doc_silhouette
   (reads?) → checkpoint save
   → build in PASSES across ALL parts: volume (doc_form/doc_relight) → pixel
     detail per component (doc_paint_grid / doc_dump_region→edit / doc_draw op=pencil /
     op=stroke for tapered limbs & organic arcs)
   → doc_material / doc_dither_ramp
   → doc_smooth_edges (selout) / doc_outline_selective → doc_pixel_perfect
   → AUDIT: doc_critique (scorecard) · palette_report · contrast_check · snap_palette
       [+ ref_compare: iou ≥ 0.80, fix worst_cells first]
   → fix what numbers flag (or checkpoint restore) → doc_look (confirm)
   → [animate: plan stride/bob/durations → extract_to_layer parts →
      keyframe_transform about joints → add_frame copy_from for the rest
      → frame_diff/anim_audit (seam·spacing·arc·timing) → contact_sheet onion=true
      → set_frame_duration (hold contacts) → add_tag]
   → set_pivot · set_frame_boxes → export_sheet / export_gif / export_atlas
```

Iterate the audit-fix-render cycle until every gate passes. Done means the numbers
are clean, not that it "looks ok" in one glance.
