# Roadmap

atelier is a headless pixel-art editor driven over MCP: layered/animated
documents, drawing primitives, selection/region editing, render-to-PNG previews,
analysis/critique tools, and spritesheet / GIF / atlas export with tags, pivots
and palettes. What exists is documented in the README and the
[CHANGELOG](CHANGELOG.md); this file is the backlog.

## Near term

- **Free-form lasso selection** — a freehand lasso to complement the contiguous
  magic-wand (`doc_select_wand`) and the `doc_select` rect/ellipse/by-colour masks.
- **Draw-by-palette-index** — paint with palette slots so editing the palette
  recolours the art; pairs with `doc_set_palette` / `doc_palette`.
- **Trimmed atlas** — trim transparent margins per frame and record the trim
  offset, for tighter textures.
- **Layer groups** — nested/grouped layers with a group blend & opacity (needs a
  model change to the flat layer stack).
- **Normal-map export** — derive a normal map from the silhouette/height for
  engine lighting (in-editor auto-shade already ships: `doc_relight` / `doc_form`
  / `doc_rim_light`).
- **AI vision critique** — `doc_critique_vision` via MCP host sampling (the host
  runs its own model; atelier ships no weights/network/keys), to catch the
  perceptual "last 5%" the deterministic metrics and `doc_diff_map` can't.

## Export & integration

- **Aseprite `.ase` import/export** for round-tripping with the real editor.
- **Per-engine metadata presets** (Godot `SpriteFrames`, Unity, LDtk).

## Engineering

- **Integration tests** over the MCP tool layer (currently unit-tested at the
  `Document`/`Studio` level — 198 tests — plus the replayable recipes under
  `docs/examples/`, which double as end-to-end checks; the `--record` recorder
  turns a live session straight into one of those recipes).
- **Undo / history** per document — a full per-op undo stack, beyond the manual
  `doc_checkpoint` / restore that ships today.
- **End-to-end agent art eval** — an agent task-suite scored by a perceptual
  judge, to measure delivered art quality (the deterministic `quality_benchmark`
  measures engine capability, not agent output).

Nothing here is required to use atelier today; it is the backlog, not a list
of missing essentials.
