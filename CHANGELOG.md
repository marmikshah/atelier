# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/); versions follow
[SemVer](https://semver.org/). Below 2.0.0, breaking changes ship in minor
releases.

## [Unreleased]

### Added

- **`atelier agent` — the one online mode.** Drives an OpenAI-style
  chat-completions API through an agentic loop so the binary draws a task on its
  own, with no external client. It executes the model's tool calls against a
  child `atelier` stdio server, so it reuses the whole validated tool path
  (schemas, arg-checking, journaling) rather than a second copy; `doc_look`
  images are fed back to the model. Any OpenAI-compatible endpoint works via
  `--base-url`. The atelier-sprite/scene/review skills are compiled into the
  binary, so a bare `atelier agent --task "..."` needs no files — `--skill
  scene|review` picks another, `--skill-file` injects a custom one.
- Gated behind the **`agent` cargo feature**, OFF by default: a normal build and
  the daemon link no HTTP/TLS stack, and the core stays offline, keyless and
  deterministic. `OPENAI_API_KEY` comes from the env; agent mode is never part
  of a default install. Build with `cargo install --path crates/atelier
  --features agent`.

### Added

- **Three skills for Claude Code** (`.claude/skills/`), which `install.sh` offers
  to install: **atelier-sprite** (one subject), **atelier-scene** (a place), and
  **atelier-review** (judge it, don't repaint it). Both drawing skills insist on
  building in layers and fixing the region that is wrong rather than repainting
  the frame; neither prescribes a style or a palette. A test fails if a skill
  names a tool that no longer exists.

### Breaking

- **The MCP server ships no prompts.** `pixel-sprite`, `walk-cycle` and
  `seamless-tile` are gone; the skills above replace them, and go deeper than a
  prompt could. Clients other than Claude Code lose the packaged workflows —
  the tool descriptions still carry the per-tool guidance.
- **63 tools → 28.** Every tool with no caller — in any agent transcript or any
  shipped recipe — is gone, along with ~6,000 lines behind them. Removed: the
  character generators (`doc_figure`, `doc_walk`, `doc_pose_cycle`), terrain
  (`doc_autotile_set`, `doc_tilemap_assemble`, `doc_wang_tiles`), `doc_nine_slice`,
  the particle emitters (`doc_emit`, `doc_burst`), the heavy effects
  (`doc_glow`, `doc_relight`, `doc_material`, `doc_rim_light`, `doc_cast_shadow`,
  `doc_smooth_edges`, `doc_outline_selective`), the unused audits
  (`doc_set_audit`, `doc_form_audit`, `doc_colorblind_check`,
  `doc_contrast_check`, `doc_coverage_map`, `doc_ramp_validate`,
  `doc_translucency_report`, `doc_critique_vision`), the keyframe/transform tools,
  `doc_stamp_image`, `doc_extract_to_layer`, `doc_dissolve`, `doc_select_wand`,
  `doc_select_render`, `doc_perspective_guide`, `doc_set_pivot` and
  `doc_set_frame_boxes`. Pillow-shading detection survives inside `doc_critique`.
- **No more tool profiles.** `ATELIER_PROFILE` is gone from the env, the
  installer, the Docker image and the daemon manifest. All 28 tools are always
  advertised — at this size, hiding a third of them behind a flag cost more in
  confusion than it saved in context.
- **Prompts:** `game-asset-set` is removed (it was built from deleted tools).
  `walk-cycle` now animates by repainting each pose, which is what agents
  actually do.

### Added

- **Every document journals itself.** The calls that built a document are
  recorded beside it as `recipe.jsonl`, on by default — so `atelier replay <id>`
  rebuilds anything you have ever drawn, and "art is a recipe" stops depending on
  having known to pass a flag first. `atelier library` shows each document's step
  count.

### Changed

- **Recipes read JSON Lines.** One `{tool, args}` per line, appended: O(1) per
  call instead of rewriting the whole file, and a killed session leaves every
  completed line intact. The authored `{name, description, steps}` recipes in
  `docs/examples` still work — `replay` takes either.
- **Reads are no longer recorded.** `doc_look`, `doc_info`, the audits and
  `doc_ref op=compare` rebuild nothing, so they are not part of a recipe. This
  also removes the see-and-fix loop's noise from `--record` output.
- `--record` is now the cross-document *session* capture; per-document
  provenance is the journal's job.

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
