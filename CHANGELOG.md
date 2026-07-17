# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/); versions follow
[SemVer](https://semver.org/). Below 2.0.0, breaking changes ship in minor
releases.

## [Unreleased]

Nothing yet.

## [1.5.0] — 2026-07-17

### Added


- **`frames` on doc_batch**: apply the same op list to several frames in one
  call — the repeated 1px fix on a static layer no longer costs one round-trip
  per frame (the last multi-frame gap the showcase agents hit).
- **The loop-seam audit calibrates against the loop's own motion.**
  `doc_anim_audit mode=seam` now also reports `typical_step_changed` (median
  changed pixels of adjacent steps) and `wrap_vs_typical` — whole-body motion
  repaints most pixels every step, so the absolute `seam_score` read as a pop
  even when the wrap was exactly as busy as any mid-loop step. A ratio near 1
  is called out as healthy; ≥2 as a likely pop.
- **`erase: true` on every draw op** (doc_draw and doc_batch): the shape becomes
  an eraser — every pixel it touches goes transparent. Any pencil/line/ellipse/
  polygon/fill can punch a hole, which no colour trick could (drawing
  `[0,0,0,0]` is a no-op under source-over). Every FX agent in the showcase
  benchmark wanted this.
- **`bg` on doc_look** (`checker` | `dark` | `white`): matte transparency for
  viewing. Most viewers matte on white, which made white-hot FX pixels
  invisible — two showcase agents flagged it.
- **`count` on doc_frame op=add**: append several frames in one call instead of
  N identical round-trips.
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
- **Three skills for Claude Code**, which `install.sh` offers
  to install: **atelier-sprite** (one subject), **atelier-scene** (a place), and
  **atelier-review** (judge it, don't repaint it). Both drawing skills insist on
  building in layers and fixing the region that is wrong rather than repainting
  the frame; neither prescribes a style or a palette. A test fails if a skill
  names a tool that no longer exists. They are a typed registry
  (`crates/atelier/skills`): Rust owns the metadata and the per-consumer
  renderers, the prose stays markdown. `atelier skills install` writes the
  Claude `SKILL.md` files; `atelier skills show <name>` prints one.
- **Every document journals itself.** The calls that built a document are
  recorded beside it as `recipe.jsonl`, on by default — so `atelier replay <id>`
  rebuilds anything you have ever drawn, and "art is a recipe" stops depending on
  having known to pass a flag first. `atelier library` shows each document's step
  count.

### Fixed


- **Stringified params are revived.** Strict tool-call clients serialize params
  the flattened open schemas leave untyped as STRINGS — `color: [255,0,0]`
  arrived as `"[255, 0, 0]"` (rejected) and `dx: 2` as `"2"` (silently
  defaulted to 0, the doc_fx op=shift "no-op" every showcase model worked
  around). doc_draw/doc_fx/doc_export/doc_batch now parse those back; `text`
  stays prose.
- doc_anim_audit's timing note named a tool that no longer exists
  (`doc_set_frame_duration`) — it now points at `doc_frame op=duration` and
  says to skip the advice when the brief fixes the timing.
- doc_critique's orphan/jaggies notes acknowledge FX sprites: deliberate
  sparks/embers are legitimate orphans, and small curves on a locked palette
  always keep some step corners — judge by eye, don't chase zero.
- The "no pixels changed" warning no longer implies an error when the edit was
  simply a no-op.
- **Replay remaps document ids.** `atelier replay <id>` into a store where the
  id already exists used to draw every step onto the LIVE original (and double
  its journal) while the fresh copy stayed blank; journals of collision-suffixed
  documents (`hero-2`) could never replay at all. The journal now stamps the
  minted id onto its `doc_create` line and replay rewrites
  `doc_id`/`set_doc`/`from_doc`/`ids` through a recorded→minted map.
- **Pastes replay self-contained.** A journaled `doc_region op=paste` embeds the
  pixels it used, so a rebuild no longer depends on (or clobbers) the live
  process clipboard — cross-document copies included.
- **The child server always speaks stdio.** `atelier replay` and
  `atelier agent` scrub `ATELIER_HTTP`/`ATELIER_RECORD` from the child's
  environment; an exported `ATELIER_HTTP` used to make the child bind HTTP and
  the handshake hang forever. Responses are also read under a timeout now, and
  both commands share one `StdioClient` instead of two copies of the plumbing.
- **Journal order can no longer diverge from execution order** under concurrent
  HTTP sessions: mutations hold one order lock across dispatch and append.
- **`--record` truncates a reused path** instead of concatenating two sittings
  into one unreplayable file, and records `delete_doc` so create/delete/create
  sittings replay with the right ids.
- **One journal read policy.** `atelier library` no longer reports "N steps /
  replayable" for a journal `atelier replay` refuses: both tolerate a torn
  final line (announced on stderr) and error on mid-file corruption; the
  listing says "corrupt journal". A journaled `doc_ref op=set` whose reference
  image has moved warns and continues instead of aborting the rebuild.
- **Agent-loop edges:** transient API failures (429/5xx) retry with backoff
  instead of discarding the run; malformed tool-call JSON gets an explicit
  error back instead of running the tool with `{}`; an empty assistant message
  is a failure, not success; `http://[::1]` is accepted as loopback; a flag
  following `--task` is a missing value, not the task.
- The examples in `docs/examples/` are now genuinely integration tests:
  `make test` replays each through the real binary into a throwaway store.

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
  A pre-trim recording or journal that used a deleted tool no longer replays: the
  run stops cleanly at that step with the unknown-tool error. Frame pivots and
  collision boxes lose their storage and export slots too — the only setters
  were the deleted tools, so for any new document they were permanently empty.
- **No more tool profiles.** `ATELIER_PROFILE` is gone from the env, the
  installer, the Docker image and the daemon manifest. All 28 tools are always
  advertised — at this size, hiding a third of them behind a flag cost more in
  confusion than it saved in context.
- **Prompts:** `game-asset-set` is removed with the rest of the prompts (it
  was built from deleted tools). The skills that replaced the prompts animate
  walk cycles by repainting each pose, which is what agents actually do.

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
