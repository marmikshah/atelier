# Roadmap

atelier is a headless pixel-art editor driven over MCP: layered/animated
documents, drawing primitives, selection/region editing, render-to-PNG previews,
analysis/critique tools, and spritesheet / GIF / atlas export with tags, pivots
and palettes. What exists is documented in the README; this file is the backlog.

## Shipped — the art-quality pass

The 30-agent art-quality review (`docs/ART-QUALITY-REVIEW.md`) landed 22 craft
tools (see TOOLS.md → "World-class craft"), clearing several roadmap items:

- **Cel transform** — `doc_transform_cel` (in-place rotate/scale/skew, RotSprite).
- **Contiguous magic-wand** — `doc_select_wand` (perceptual OKLab flood → mask).
- **Undo / history** — `doc_checkpoint` (snapshot/restore/diff/prune).
- **Auto-shade from silhouette** — `doc_relight` (height→normal key/fill/rim).
- **Grid / coordinate overlay + inline preview** — `doc_look`.
- **Multi-frame contact sheet** — `doc_contact_sheet`.
- Plus OKLab colour (`doc_make_perceptual_ramp` / `doc_harmony_palette` /
  `doc_snap_palette`), selout (`doc_smooth_edges`), `doc_critique`, materials,
  iso boxes, perspective guides, UI panels, reference import, FX bursts.

## Near term

- **Lasso selection** — free-form polygon mask (magic-wand + rect/ellipse/colour
  already shipped).
- **Draw-by-palette-index** — paint with palette slots so editing the palette
  recolours the art (`doc_snap_palette` mitigates today; true indexed buffers
  are the full fix).
- **Trimmed atlas** — trim transparent margins per frame and record the trim
  offset, for tighter textures.
- **Layer groups** — nested/grouped layers with a group blend & opacity (needs a
  model change to the flat layer stack).
- **Normal-map export** — emit a normal/height map (relight derives them
  internally; this exposes them for engine lighting).
- **Warp / mesh** — homography quad-projection (iso faces) and lattice
  squash/liquify, beyond `doc_transform_cel`'s affine.
- **Bitmap fonts** — loadable fonts + `.fnt`/BMFont export beyond the built-in
  3×5 and `doc_panel`/`doc_text`.
- **Rig / cutout animation** — named parts + per-part pivots (a data-model
  change) for pose-to-pose and part-swap.

## Export & integration

- **Aseprite `.ase` import/export** for round-tripping with the real editor.
- **Per-engine metadata presets** (Godot `SpriteFrames`, Unity, LDtk).

## Engineering

- **Integration tests** over the MCP tool layer (currently unit-tested at the
  `Document`/`Studio` level — 114 tests — plus the replayable recipes under
  `docs/examples/`, which double as end-to-end checks; the `--record` recorder
  turns a live session straight into one of those recipes).
- **Undo / history** per document, so a bad op can be reverted without rebuilding
  the cel.

Nothing here is required to use atelier today; it is the backlog, not a list
of missing essentials.
