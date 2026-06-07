# Roadmap

atelier is a headless pixel-art editor driven over MCP: layered/animated
documents, drawing primitives, selection/region editing, render-to-PNG previews,
analysis/critique tools, and spritesheet / GIF / atlas export with tags, pivots
and palettes. What exists is documented in the README; this file is the backlog.

## Near term

- **Free-form selection** — lasso and contiguous magic-wand, extending the
  `doc_select` rect/ellipse/by-colour masks already shipped.
- **Cel transform** — scale/rotate a cel or selection in place (the stamp path
  already scales/rotates imported images; this brings it to existing cels).
- **Draw-by-palette-index** — paint with palette slots so editing the palette
  recolours the art; pairs with `doc_set_palette` / `palette_ramp`.
- **Trimmed atlas** — trim transparent margins per frame and record the trim
  offset, for tighter textures.
- **Layer groups** — nested/grouped layers with a group blend & opacity (needs a
  model change to the flat layer stack).
- **Normal-map / auto-shade** — derive a normal map or auto top-light from the
  silhouette/height for engine lighting.

## Preview ergonomics (the agent's feedback loop)

- **Grid / coordinate overlay** on `doc_render` (opt-in) so the agent reads pixel
  coordinates straight off the preview.
- **Multi-frame contact sheet** — all frames in one PNG for a whole-animation
  glance. (Onion-skin / region / tiled / thumbnail previews already shipped.)

## Export & integration

- **Aseprite `.ase` import/export** for round-tripping with the real editor.
- **Per-engine metadata presets** (Godot `SpriteFrames`, Unity, LDtk).

## Engineering

- **Integration tests** over the MCP tool layer (currently unit-tested at the
  `Document`/`Studio` level — 86 tests — plus the replayable recipes under
  `docs/examples/`, which double as end-to-end checks).
- **Undo / history** per document, so a bad op can be reverted without rebuilding
  the cel.

Nothing here is required to use atelier today; it is the backlog, not a list
of missing essentials.
