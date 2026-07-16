---
name: atelier-sprite
description: Draw a single pixel-art subject — a character, creature, vehicle, prop, item or effect — as a layered, optionally animated atelier document. Use when asked to make a sprite, icon, or any one discrete object, still or animated. Builds it in parts on separate layers, looks at every pass, and fixes the specific thing that is wrong rather than redrawing. Requires the atelier MCP server. For backgrounds and full scenes use atelier-scene; to judge finished art use atelier-review.
---

# Drawing one subject

A sprite is one subject on a transparent field. You are not painting a picture —
you are building an object that a game will move, tint, and cut apart. Build it
that way.

Nothing here prescribes a style, a palette, or a canvas size. Those come from the
request. What follows is how to work.

## The two rules

**1. Parts live on their own layers.** Never build a subject as one flat cel.
A cel is the atom you can edit, animate and throw away independently — a subject
on one layer is a subject you can only fix by redrawing.

Split by what moves or reads separately, not by colour:

| subject | layers |
|---|---|
| a character | body · head · each limb that swings · held item |
| a creature | body · head · tail · wings |
| a vehicle | chassis · each wheel · exhaust/FX |
| a prop | body · the moving part · the glow/FX |

If a part will animate, it gets a layer. If a part might be recoloured or
swapped, it gets a layer. When in doubt, split — merging later is one
`doc_layer op=merge_down`; separating later is a redraw.

**2. Fix the region, never the frame.** When something is wrong, do not repaint
the cel. Name the failing area, fix exactly that, and look again.

- `doc_look` tells you *something* is wrong; `doc_dump_region` tells you *which
  pixels* — read the actual grid before you touch it.
- Confine the fix: `doc_select` the area, or aim `doc_draw`/`doc_batch` at the
  specific coordinates on the specific layer.
- One problem per pass. Two fixes at once and you cannot tell which worked.

A redraw throws away everything that was already right. It is almost never the
cheapest fix, and it is how a sprite oscillates instead of converging.

## The loop

```
doc_create → lock a palette → silhouette → look → block → look → detail → look → audit → export
```

1. **`doc_create`**, then add a layer per part (`doc_layer op=add`).
2. **Lock a palette** (`doc_palette op=set`, or `op=generate` then set). Every
   later op stays inside it. Which colours is the request's business, not this
   skill's.
3. **Silhouette first, on the body layer.** Block the shape in one flat colour.
   Run `doc_silhouette` — if the subject does not read as itself in pure black,
   the shape is wrong. Fix it now, before a single detail. A detailed sprite with
   a broken silhouette is wasted work.
4. **Block the big masses** per layer with `doc_batch` (many ops, one call).
   `doc_draw` is the single-op form — use it for a one-off, `doc_batch` for a
   burst.
5. **`doc_look` after every burst.** It hands the frame back as an image. Look at
   it and say what is wrong in words before you touch anything. If you cannot
   name the problem, you are guessing.
6. **Detail last**, and only where it earns attention — the focal area (usually
   the head/face for a character, the readable face of an object).
7. **Audit**: `doc_critique` for the failure modes you cannot see,
   `doc_palette op=report` for drift and near-duplicates.
8. **`doc_export`**.

Iterate 4–7. Stop when the next fix would not change how it reads at 1×.

## Animating

- Duplicate, then edit what moves: `doc_frame op=add copy_from=<n>`.
- **Repaint the moving part on its own layer.** The body layer should be
  untouched between frames if the body does not move. This is the whole reason
  for the layer split.
- There is no pose interpolation. There should not be — a cross-fade ghosts a
  limb, it does not move it.
- `doc_contact_sheet` shows every frame in one grid — the flip-test.
- `doc_frame_diff` between neighbours: only the parts you meant to move should
  have changed. Anything else is a mistake you cannot see by eye.
- `doc_anim_audit` for spacing and the loop seam. `doc_add_tag` names the range
  so the exported sheet carries it.

## Before you touch anything risky

`doc_checkpoint op=save`. Quantising, palette snapping and large fills are hard
to undo by hand and trivial to roll back.

## Failure modes

- **Detailing before the silhouette reads.** The most expensive mistake here.
- **One flat layer.** You will pay for it the moment anything needs to move.
- **Redrawing the cel to fix one wrong region.**
- **Drawing blind** — several ops between `doc_look`s, then guessing which
  broke it.
- **Fixing what you did not diagnose.** `doc_dump_region` first, then edit.
