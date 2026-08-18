# Drawing a place

*Every tool named below is one call: `atelier call <tool> '<json-args>'` from a shell — or the same-named tool over MCP. Run `atelier init` once per project to keep its art in its own `./.atelier`.*

A scene is not a big sprite. A sprite is one subject on nothing; a scene is
several subjects in a space, and it succeeds or fails on whether that space
reads — depth, light, and where the eye lands.

Nothing here prescribes a style, a palette, a mood or a canvas size. Those come
from the request. What follows is how to work.

## The two rules

**1. Depth bands live on their own layers.** Never build a scene as one flat cel.
Split by distance from the viewer — that is what a scene *is*, and it is the
split that makes every later decision easy:

```
foreground   framing, in shadow, often cropped
midground    the subject — where the eye should land
background   the setting
sky / far    the backdrop, flattest and lowest contrast
```

Give each band a layer, back to front. Then:

- A band can be dimmed, shifted or replaced without touching the others.
- Parallax is free: move one layer per frame.
- Atmospheric perspective becomes a per-layer decision instead of a per-pixel
  argument — distance flattens contrast and pulls colour toward the sky.

Anything that will move, glow, or be edited on its own gets its own layer on top
of that.

**2. Fix one band, never the frame.** When a scene is wrong, the instinct is to
repaint. Don't. Name which band fails, fix that band's layer, look again.

- Explicit coordinates confine the change; `doc_region` handles a
  self-contained clear or move.
- `doc_dump_region` reads the actual pixels of the area you doubt.
- One band per pass. A scene has too many variables to change two at once and
  still know what happened.

## The loop

```
doc_new → layers back-to-front → lock a palette → value blocking → look → band by band → look → detail the focal area → audit → export
```

1. **`doc_new`**, capture its returned `doc_id`, and pass that id explicitly on
   every later document call. Then add a layer per band (`doc_layer op=add`),
   back to front.
2. **Lock a palette** (`doc_palette op=set` / `op=generate`). Which colours is
   the request's business.
3. **Block values before colour.** Flat masses per band, darkest to lightest.
   `doc_look mode=notan` collapses the frame to values — if the composition does
   not read as flat shapes, no amount of detail will save it.
4. **Decide the light before you shade anything.** Where is it, how many
   sources, what temperature. Then apply it *consistently*: every object lit on
   the same side, every shadow cast away from the same source. Inconsistent light
   is the single loudest tell that a scene was assembled rather than seen.
5. **Work band by band, back to front**, with one `doc_draw` or `doc_fx`
   operation per call. `doc_look` between bands.
6. **Detail only the focal area.** The eye lands in one place. Detail everywhere
   is detail nowhere, and it flattens the depth the bands just bought you.
7. **Audit**: `doc_critique`, `doc_palette op=report`.
8. **`doc_export`**.

## Making it read

- **Squint test**: `doc_look mode=notan`. The focal area should be the strongest
  value contrast in the frame. If two bands share a value mass, they merge.
- **Depth**: contrast and saturation fall off with distance. If the background
  is as punchy as the midground, the space collapses.
- **Texture follows form.** Random speckle is not texture — it is noise, and it
  reads as noise at every zoom.
- **A gradient is a ramp, not a blur.** `doc_dither_ramp` graduates across a
  locked palette; dithering between two ramp steps is how pixel art makes a
  gradient without inventing colours.
- **Empty space is a choice.** An empty foreground is dead weight; a foreground
  in shadow is framing.

## Animating a scene

- Ambient motion only, on its own layer: a flicker, drifting particles, a slow
  parallax shift. The scene should not lurch.
- `doc_frame op=add copy_from=<n>`, then repaint only the moving layer.
- `doc_frame_diff` proves only that layer changed.
- `doc_anim_audit` mode=`seam` for the loop wrap.

## Before you touch anything risky

`doc_checkpoint action=save`. A palette snap or a full-canvas fill across a
multi-band scene is exactly the op you will want to undo.

## Failure modes

- **One flat cel.** Every fix becomes a repaint; there is no depth to tune.
- **Detail before value.** A scene that fails the squint test cannot be rescued
  by detail.
- **Light with no source** — a lantern that lights nothing, a shadow pointing the
  wrong way. If you drew a light, it must fall on something.
- **Uniform detail.** No focal point, so the eye slides off.
- **Speckle standing in for texture.**
- **Repainting the frame** when one band was wrong.
