# atelier — Round-2 Tool-Consolidation Plan (reconciled)

> 33-agent review (2026-06-20): capability signatures over all 105 tools →
> fusion/redundancy clustering → per-candidate adjudication (argue both sides) →
> ranked plan → adversarial critic. 18 candidates → 4 fuse, 5 improve, 0 clean
> remove, 9 keep. This file is the maintainer's reconciliation of plan + critic.

## Guiding principle (from the critic)

For an **LLM-driven** tool surface, an agent picks a tool by name+description match
*before* it reads a param schema. So:

> **Fuse only where the mode-param is behaviourally neutral over a shared kernel.
> Never merge where a mode changes the meaning of another param or the call
> protocol** — that turns a free name-match into a silent wrong-mode call.

Tool-count reduction is NOT the goal; surface *clarity* and *param consistency* are.

## SHIP

| # | Action | What | Why it's safe | Effort |
|---|---|---|---|---|
| 1 | **bugfix** | batch `gradient` op now re-snaps on a locked palette (parity with standalone `doc_gradient`) | latent correctness bug — batch path silently skipped the snap | S |
| 2 | **fuse** `doc_export_anim` | `doc_export_gif` + `doc_export_apng` → `format: gif\|apng`. Old names kept as deprecated aliases | codec-equal = behaviourally-neutral mode; one shared op | S |
| 3 | **fuse** `doc_palette` | `palette_ramp` + `doc_make_perceptual_ramp` + `doc_harmony_palette` → `scheme: mono\|complementary\|triadic\|analogous\|split\|tetradic`. Aliases kept | all funnel one `make_ramp_oklch` kernel; `scheme` is a true peer; mono *gains* sat_curve/anchor/validation it was arbitrarily denied | M |

Back-compat: every fuse ships the old tool names as thin deprecated alias handlers
for one release. These are public MCP tools; no hard breaks.

**SHIPPED** (dev/v1.2.0): #1 batch-gradient snap parity bug · #2 `doc_export_anim`
· #3 `doc_palette`. **HARD BREAK** (no users yet): the 5 superseded tools
(`doc_export_gif`, `doc_export_apng`, `palette_ramp`, `doc_make_perceptual_ramp`,
`doc_harmony_palette`) and their studio methods/structs were REMOVED outright
(not kept as aliases). Prompts, recipes, TOOLS.md updated to the new names.

**Deferred to its own release (#4 `doc_look` fold):** add `tile`/`out_path` to
`doc_look` and fix `look_stats` to report the analysis channel, then retire
`doc_render`/`doc_render_value`. Deferred because the critic flagged the
`look_stats` rework as the riskiest item (a silent wrong-answer regression if the
sat/hue band stats aren't migrated correctly), and `doc_render`/`doc_render_value`
still provide `tile`/`out_path`/band-stats meanwhile — no capability is lost.

## REJECT (do NOT do — flagged traps / churn / discoverability loss)

| Candidate | Why rejected |
|---|---|
| `doc_select` shape=wand | `tolerance` means RGBA-sum (color) vs OKLab ΔE (wand); default flips 0→16. Same param name, different physics, no error. Keep `doc_select_wand` separate (or, if ever merged, rename the wand param `delta_e`). |
| `doc_set_frame_meta` (pivot+boxes) | tri-state omit/null/value protocol = negative cognitive load for an agent; no clean alias for the old omit-pivot=clear. Keep separate. |
| remove `doc_get_pixel` | clear, discoverable named tool ("read one pixel"); already fixed to read the flattened composite. Folding into `dump_region`'s 1×1 case trades name-match for a mode gamble. Keep. |
| fold `doc_layer_ops`←`set_layer`/`add_layer` | verified default trap (layer_ops insert defaults to BOTTOM, add_layer appends TOP → silent z-flip); and removes two discoverable named tools. Keep. |
| `doc_frame_ops` append | the plan itself calls it "additive de-dup, keep all three" — it's a feature add, not consolidation. Skip. |

## KEEP — look mergeable, are NOT (load-bearing distinctions)

`replace_color`(RGB-tolerant, batchable) vs `palette_swap`(exact, rewrites palette) ·
`clear_cel`(removes cel) vs `clear_region`(materialises canvas) ·
`move_region`(same-cel) vs `keyframe_move`(later frames, errors on same) ·
`shade`/`form`/`relight`(three different light physics) ·
`dither`(density halftone) vs `dither_ramp`(gradient shader) ·
`outline`(flat keyline+AA) vs `outline_selective`(fill-derived contour) ·
`drop_shadow`(under, dark) vs `glow`(over, bright) ·
`flip`(lossless, batchable) vs `transform_cel`(lossy affine) ·
`seam_report`(1-D edge scan) vs `anim_audit seam`(2-D area diff).

## Deferred hygiene (additive, not consolidation)

- Route `doc_relight` + `doc_outline_selective` through `edit_masked` (honour the
  active selection) and add them to the batch op table.
- Cross-link the `shade`/`form`/`relight` rung descriptions so the agent picks the
  right escalation tier.
- Region-mobility family (`copy`/`cut`/`paste`/`move`/`extract`/`stamp`) — the
  critic flagged it as an un-reviewed cluster, but round-1 already kept these as
  genuine wrappers; revisit only if it proves confusing in practice.
