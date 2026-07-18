# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/); versions follow
[SemVer](https://semver.org/). Below 2.0.0, breaking changes ship in minor
releases.

## [Unreleased]

### Fixed

- **Coordinate/scale hardening** — one absurd parameter can no longer wedge the
  server or abort it: `rect`/`ellipse` fill loops and `line` are clipped to the
  canvas (the line's `(x1-x0)` span also overflowed i32), `brush`/`scatter`
  sizes clamp to the canvas, `bevel` depth, `noise` octaves and `box_blur`
  radius are bounded, and `doc_look tile`, `contact_sheet scale` and
  `doc_frame_diff scale` go through the same 1..=16 clamp as every other
  resize (each sized a buffer straight from caller input).
- **i32 overflow sweeps** — `shift` wrap, `symmetry`, `move_region`,
  `doc_select` ellipse, `box_iso`/`panel` geometry now compute in i64/f64 and
  saturate: no debug panic, no release wrap.
- **Checkpoint `restore` stages then swaps** — a failed restore no longer
  deletes the live document it was meant to rescue.
- **Deterministic palettes** — median-cut ties break on the full colour, so a
  HashMap-ordered input (import, reference palettes) yields the same palette
  every run; long GIF frame durations clamp to the format ceiling instead of
  wrapping.
- **Multi-frame `doc_batch` preflights frames** — a bad target late in the list
  used to leave earlier frames mutated but the call unjournaled, silently
  diverging the document from its recipe.
- **Colour validation is uniform** — typed tool paths (`doc_palette`,
  `doc_select`, `box_iso`, `panel`, `dither_ramp`, `paint_grid` legends) reject
  malformed/out-of-range colours instead of truncating them via `as u8`;
  batch `opacity`/`intensity`/`shadow_opacity` reject >255 likewise.
- `dither_ramp` errors on an unknown pattern (it silently fell back to bayer8);
  core GIF/APNG scale math is checked like the spritesheet's; `doc_look`
  reports the scale it actually applied.
- **systemd units quote paths** — an `ATELIER_HOME` or binary path containing
  spaces no longer installs a service that can't start (`"`/`%` rejected).
- Stale cel files are swept on save (cleared cels / deleted layers and frames
  used to linger); `doc.json` writes are temp-then-rename.
- Doc drift: ARCHITECTURE.md (raster contents, LOC figures, binary deps),
  benchmark task count (8 → 10), module docs.

### Changed

- **CHANGED OUTPUT — GIF palette for palette-less documents.** The shared
  global palette is now a frequency-weighted median cut over deduped, sorted
  colour counts (was a flat per-pixel cut). Deterministic on every run and far
  less memory, but it is a different cut: re-exported GIFs can quantize
  slightly differently from 1.5.0. APNG output is unaffected (it never
  quantizes).
- **Saves write only dirty cels** — an edit used to re-encode and rewrite every
  cel in the document per call; structural re-keys and per-op edits track a
  dirty set, and a round-trip with no edits writes nothing but `doc.json`.
- **The batch-op registry is one table** — dispatch, validation keys and the
  doc_draw/doc_fx split were three hand-synced lists with tests existing only
  to catch drift; a new op is now one `OPS` entry (`DRAW_OPS`/`FX_OPS` consts
  became `draw_ops()`/`fx_ops()` fns in atelier-core).
- Release profile: thin LTO, one codegen unit, stripped symbols.

### Added

- A deterministic structural fuzzer (seeded op sequences over the layer/frame
  lifecycle, asserting the meta↔cel lock-step after every op) and regression
  tests for every fix above.

## [1.5.0] — 2026-07-17

### Breaking

- **63 tools → 28.** Every tool with no real caller is gone (generators, terrain,
  heavy effects, unused audits, keyframe/pivot/box tools). Pre-trim recordings
  that used a deleted tool stop cleanly at that step.
- **MCP prompts removed** — the three skills replace them.
- **`ATELIER_PROFILE` removed** — all 28 tools are always advertised.

### Added

- **`atelier agent`** — the one online mode: drives any OpenAI-compatible API to
  draw a task by itself, against the same validated tool path. Behind the
  `agent` cargo feature, OFF by default; a normal build links no network stack.
- **Every document journals itself** (`recipe.jsonl`, on by default, mutations
  only) — `atelier replay <id>` rebuilds anything ever drawn.
- **Three Claude Code skills** (sprite / scene / review), baked into the binary
  and offered by the installer.
- `erase: true` on every draw op — any shape erases to transparent.
- `frames` on doc_batch — one op list applied to several frames in one call.
- `count` on doc_frame op=add — append N frames at once.
- `bg` on doc_look (checker | dark | white) — matte transparency so light
  pixels stay visible.
- Loop-seam audit reports `wrap_vs_typical`, calibrated against the loop's own
  motion — whole-body animation no longer reads as a pop.

### Fixed

- Replay remaps document ids — rebuilding into a live store no longer draws on
  the original; collision-suffixed journals replay.
- Journaled pastes embed their pixels — cross-document copies replay
  self-contained.
- Replay/agent child servers always speak stdio (an inherited `ATELIER_HTTP`
  used to hang the handshake); responses read under a timeout.
- Journal order matches execution order under concurrent sessions; `--record`
  truncates a reused path and captures `delete_doc`; one read policy for
  library and replay (torn final line tolerated aloud, corruption errors).
- Stringified tool params are revived — strict clients no longer break colour
  arrays or silently zero `dx`/`dy`.
- Agent loop: 429/5xx retry with backoff, malformed tool-call JSON reported
  back, empty reply is a failure, IPv6 loopback parses.
- Audit strings: stale tool names removed; critique acknowledges FX sprites.
- `docs/examples` recipes are real integration tests (`make test` replays them).

### Changed

- Recipes are JSON Lines; reads are never recorded. `--record` is the
  cross-document session capture, the journal is per-document provenance.
- Full showcase redrawn on this build — 4 models × 10 frozen briefs, verified
  from disk, with a whole-grid view on the site.

## [1.4.0] — 2026-07-16

20 core / 63 full tools (was 30 / 75). A CLI that works without the Makefile.

### Breaking

- Tools fused into op hubs; old names removed, not aliased:

  | now | was |
  |---|---|
  | `doc_palette op=set\|snap\|swap\|report\|sync` | `doc_set_palette`, `doc_snap_palette`, `doc_palette_swap`, `doc_palette_report`, `doc_set_palette_sync` |
  | `doc_ref op=analyze\|compare\|diff` | `doc_ref_analyze`, `doc_ref_compare`, `doc_diff_map` |
  | `doc_export op=all\|atlas` (omit `doc_id`) | `export_all`, `export_atlas` |
  | `doc_draw op=box_iso\|panel` | `doc_box`, `doc_panel` |

- Default profile advertises 20 tools; `doc_select`, `doc_contrast_check`,
  `doc_frame_diff` and `doc_set_audit` are full-only. Discovery filter only —
  every tool still executes.
- `doc_paint_grid`, `doc_stamp_image`, `doc_palette op=snap` and
  `doc_ref op=import` return text, not preview images. `doc_look` is the only eye.
- Unknown `mode` (`doc_select`, `doc_select_wand`), `outside`
  (`doc_tilemap_assemble`) and `frames` (`doc_extract_to_layer`) values now
  error instead of silently defaulting.
- `atelier install` / `status` / `uninstall` replace `atelier service <cmd>`.
- The Makefile builds, tests and lints only — use the binary's subcommands.

### Added

- `atelier library` — list and prune the document store.
- `atelier tools [--html]` — the tool surface, or the reference page.
- Docker image `ghcr.io/marmikshah/atelier` (amd64 + arm64).
- `install.sh --source` (build this checkout), `--yes` (non-interactive).

### Security

- `doc_checkpoint` accepted ids like `../../../../x` and deleted directories
  outside the store. Ids must now match the `cp<n>` form the tool mints.

### Fixed

- `doc_batch` was uncallable from strict tool parsers — its schema emitted
  `items: true`, failing the whole tool list.
- Off-canvas strokes panicked; export `scale` was unbounded (`scale=64` targeted
  ~256 GB); a panic poisoned the studio lock and bricked every later call.
- `bucket_fill` / `replace_color` ignored alpha and ate black outlines.
- `clear_cel` reported success for a layer that does not exist.
- `doc_set_audit` reported `value_range: [255, 0]` on an empty document.

### Docs

- `cargo install --path .` never worked (virtual manifest) — use
  `cargo install --path crates/atelier`.
- New hero image and logo; light theme meets WCAG AA; external contributions
  closed until 2.0.0.

## [1.3.0] — 2026-07-11

The game layer: a game is a *set* of documents, and the tools can now see it.
75 tools.

### Added

- `doc_set_audit` — audit N documents as one game (palette union, near-duplicate
  colours, scale outliers, value range, missing pivots).
  `doc_set_palette_sync` broadcasts one palette across the set.
- `doc_form_audit` — infers light direction per form and flags pillow-shading.
  Also wired into `doc_critique`.
- `doc_cast_shadow` — a projected ground shadow, optionally clipped to a
  `receiver_layer`.
- `doc_pose_cycle` — one pose in, a tagged cycle out: `idle` / `run` / `jump` /
  `attack` / `hurt`.
- `doc_autotile_set` (47-blob set + neighbour masks) and
  `doc_tilemap_assemble` (see the map, not just the tile).
- `doc_nine_slice`, `doc_emit`, `doc_colorblind_check`,
  `doc_fx op=gradient_map`, the `game-asset-set` prompt.
- `doc_export op=sheet meta=standard` — the sprite-JSON sidecar engines parse.
- `list_docs` gains `prefix` / `contains`.

### Security

- Hardened the untrusted-input boundary: images over 64 MP rejected at the
  header probe; export scale clamped; `Document::load` refuses `../` cel paths;
  the daemon installer rejects control characters and escapes the launchd plist.

### Changed

- Continuous-tone FX (`blur`, `drop_shadow`, `bevel`, `form`, `shade`) re-snap
  to the locked palette by default — opt out with `snap:false`.
- Destructive `doc_layer` / `doc_frame` / `doc_region` ops require an explicit
  index or rect. Unknown easing / method / axis / mode strings now error.
- Batch `drop_shadow` strength renamed to `shadow_opacity` (it collided with the
  batch-wide `opacity`).
- **Library API (breaking):** single-op `Studio` wrappers are gone — use
  `doc_draw` / `doc_fx` / `doc_batch`. `look` takes `LookOptions`. The MCP
  surface is unchanged.
- `install.sh` asks core vs full and bakes it into the daemon manifest.

### Removed

- The `/gallery`, `/playground` and `/live` web views. The HTTP transport serves
  only `/mcp`.

### Fixed

- Stroke poses were double-quantized, collapsing sub-pixel motion — walk cycles
  glide instead of stepping.
- `burst` / `emit` / `material` panicked on an empty `ramp`.
- The no-home fallback rooted the store at a relative `./.atelier`.

## [1.2.0] — 2026-06-28

Drawing quality and consolidation: 65 tools, down from 105.

### Added

- `ATELIER_PROFILE` tool profiles — filters discovery only; every tool still
  executes.
- `doc_stroke` — an anti-aliased capsule-union ribbon with per-vertex taper,
  connected by construction.
- `doc_figure` / `doc_walk` — a connected humanoid from named joints; a walk
  cycle via 2-bone analytic IK.
- `doc_diff_map` — per-pixel signed OKLCh error map plus the worst pixels, each
  with a fix direction.
- `doc_rim_light`, `doc_palette` (OKLCh ramp / scheme generator).
- `doc_critique_vision` — the *host* runs the vision model; atelier ships no
  weights, makes no network call, holds no keys.

### Changed

- Op-dispatch fusion. Each old tool became the matching `op` — `doc_pencil` →
  `doc_draw op=pencil`:

  | now | absorbs |
  |---|---|
  | `doc_draw op=…` | `pencil`, `line`, `rect`, `ellipse`, `polyline`, `polygon`, `stroke`, `fill`, `fill_cel`, `gradient`, `scatter`, `noise`, `text` |
  | `doc_fx op=…` | `blur`, `outline`, `drop_shadow`, `bevel`, `shade`, `form`, `dither`, `pixel_perfect`, `flip`, `shift`, `symmetry`, `quantize`, `replace_color`, `adjust` |
  | `doc_layer op=…` | `doc_add_layer`, `doc_set_layer`, `doc_layer_ops` |
  | `doc_frame op=…` | `doc_add_frame`, `doc_set_frame_duration`, `doc_frame_ops` |
  | `doc_region op=…` | `doc_copy_region`, `doc_cut_region`, `doc_paste`, `doc_move_region`, `doc_clear_region` |
  | `doc_export op=…` | `doc_export_sheet`, `doc_export_anim`, `doc_export_tileset` |
  | `doc_ref op=set\|import` | `doc_set_reference`, `doc_import_clean` |

  `doc_glow`, `doc_wang_tiles`, `doc_stamp_image` and `doc_extract_to_layer`
  stay separate.
- RotSprite resampling for rotation (`doc_transform_cel`,
  `doc_keyframe_transform`, `doc_stamp_image`) — no NN fringe.
- OKLCh ramps are gamut-mapped instead of per-channel clipped.
- Alpha-aware palette snap (`AlphaSnap` Preserve / Opaque / Flatten).

### Removed

Hard removals, no aliases: `doc_export_gif` / `doc_export_apng` →
`doc_export_anim`; `palette_ramp` / `doc_make_perceptual_ramp` /
`doc_harmony_palette` → `doc_palette`; `doc_bezier` → `doc_stroke`;
`doc_get_pixel`; `doc_render` / `doc_render_value` → `doc_look`. Renamed
`doc_tween` → `doc_dissolve` — it cross-fades, it never interpolated poses.

### Fixed

- `doc_rim_light` (i32 overflow, false rim on full-bleed edges);
  `doc_diff_map` (heat colour and named fix now agree); `doc_walk`
  (world-anchored IK — no mid-stride knee inversion).
- Flood / `replace_color` tolerance is RGB-only — no anti-aliasing halos.

## [1.1.0] and earlier

See the GitHub releases for [v1.1.0](https://github.com/marmikshah/atelier/releases/tag/v1.1.0),
v1.0.1 and v1.0.0.
