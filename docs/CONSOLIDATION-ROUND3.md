# atelier — Round-3 Consolidation: op-dispatch fusion

> Goal: shrink the **loaded** tool surface ~101 → ~34 without removing a single
> capability, by fusing the **writers** into a few noun-scoped op-dispatch tools
> and keeping the **readers** discrete.

## Status

- **SHIPPED — `doc_draw`** (101 → 89). The 13 *add-a-mark* tools folded into
  `doc_draw(op, …)` over the existing `apply_op`/`batch_op_keys` dispatch; params
  verified identical, studio methods kept as library API.
- **SHIPPED — `doc_fx`** (89 → 76). The 14 *rework-existing-pixels* tools (blur,
  outline, drop_shadow, bevel, shade, form, dither, pixel_perfect, flip, shift,
  symmetry, quantize, replace_color, adjust) folded into `doc_fx(op, …)`, same
  recipe. **`doc_glow` deliberately kept separate** — its on-palette `snap` is not
  a batch op, so folding it would have regressed the bloom. The clean split is
  `doc_draw` = add marks, `doc_fx` = transform existing pixels.
- **NEXT (document-level, manual dispatch — not batch-routable):** `doc_region`,
  `doc_layer`, `doc_frame`, `doc_palette`, `doc_export`, `doc_ref`. Plus the
  non-batch cel effects (`relight`, `rim_light`, `material`, `panel`,
  `outline_selective`, `smooth_edges`, `dither_ramp`, `snap_palette`,
  `transform_cel`, `burst`) need the op vocabulary extended before they can join
  `doc_fx`.

## The premise

105 tools is too much information for the model: every tool's JSON schema loads
into context on every request (~30–40k tokens), and near-synonym tools raise
wrong-tool selection. But the fix is *not* more pairwise merges (round-2 already
took the safe ones and correctly rejected the traps) — it's a structural change
the engine is **already built for**.

`doc_batch` runs on an op vocabulary that already exists:

- **`apply_op` (document.rs:3788)** — `"line" => self.line(...)`, `"rect" => …`,
  `"ellipse"`, `"fill"`, `"gradient"`, … a name→method dispatch table.
- **`batch_op_keys` (document.rs:4254)** — a **per-op param schema** kept in
  lockstep: `"rect" => (req:[x0,y0,x1,y1,color], opt:[fill,size])`.
- Validation that rejects unknown ops / foreign keys **loudly**.

So the ~30 drawing/FX tools are a 1:1 *top-level mirror* of a validated
op-dispatch that already ships. `doc_layer_ops` and `doc_frame_ops` are already
op-dispatchers too. Fusion = expose the dispatch, delete the mirror.

## Principle: fuse writers, keep readers

- **Writers** (mutations) share a return shape (a change-ack) and the dispatch
  above. Fusing them is nearly free and low-risk.
- **Readers** (analysis) each return a *different* structured payload — that
  distinct output schema **is** their value. `doc_contrast_check → {ratio}` vs
  `doc_components → {components[]}`. Collapsing them hides what each returns, so
  they stay discrete.

## Target surface (~34 tools)

**Fused writer dispatch (8):**

| Tool | `op` values | absorbs |
|---|---|---|
| `doc_draw` | pencil · line · rect · ellipse · polygon · polyline · stroke · fill · fill_cel · gradient · dither_ramp · text · noise · scatter · paint_grid · box · figure · perspective_guide | the per-cel "add marks" vocab (= the batch draw ops) |
| `doc_fx` | blur · outline · outline_selective · drop_shadow · glow · bevel · rim_light · shade · form · relight · material · panel · dither · adjust · replace_color · quantize · snap_palette · smooth_edges · pixel_perfect · flip · shift · transform · symmetry · dissolve · burst | filters / lighting / colour / transforms over existing pixels |
| `doc_region` | select · select_wand · copy · cut · paste · move · clear · extract · stamp | rectangular-region + clipboard + selection |
| `doc_layer` | add · set · move · merge · delete · reorder | layer structure (folds `doc_layer_ops`) |
| `doc_frame` | add · clear_cel · duration · pivot · boxes · tag · reorder · delete · keyframe_move · keyframe_transform · walk | timeline + cel-slot + keyframe writes (folds `doc_frame_ops`) |
| `doc_palette` | set · swap · generate | palette set / recolour / OKLCh ramp generation |
| `doc_export` | sheet · anim · tileset · wang · atlas · all | every file-writing export |
| `doc_ref` | set · import | reference-image setup (`doc_set_reference`, `doc_import_clean`) |

`doc_batch` stays — it's the multi-op composer the whole thing is built on.

**Kept discrete — lifecycle & history (5):**
`doc_create` · `doc_info` · `list_docs` · `delete_doc` · `doc_checkpoint`

**Kept discrete — the eye & readers (20):**
`doc_look` · `doc_dump_region` · `doc_silhouette` · `doc_components` ·
`doc_coverage_map` · `doc_contrast_check` · `doc_palette_report` ·
`doc_ramp_validate` · `doc_frame_diff` · `doc_seam_report` · `doc_anim_audit` ·
`doc_critique` · `doc_critique_vision` · `doc_diff_map` · `doc_ref_compare` ·
`doc_ref_analyze` · `doc_translucency_report` · `doc_select_render` ·
`doc_contact_sheet`

≈ **8 + 5 + 20 = 33 tools** (from ~101). The ~67 removed are all writers folded
into the 8 dispatchers; no analysis capability is touched.

## Why the old "don't fuse" rule no longer binds

Round-2's rule — *the agent name-matches before reading the schema, so a mode
hides a wrong param* — was calibrated for weaker tool-use models. Current models
read param schemas and handle enums / discriminated unions well, and
`batch_op_keys` validation makes a wrong-param call **loud, not silent**. Better:
explicit per-op params *fix* the traps round-2 rejected (the `layer_ops` BOTTOM
z-flip, the `select_wand` tolerance flip) by making position/tolerance explicit
per op, instead of colliding defaults across two same-named-param tools.

## Risks & mitigations

- **Discriminated-union schema quality.** Each `op` must advertise its own
  required/optional params (a JSON-Schema `oneOf` on `op`), not a flat union bag.
  We already have the data (`batch_op_keys`) — generate the schema from it so the
  dispatch table and the advertised schema can't drift.
- **Per-op "when to use" guidance** moves from individual tool descriptions into
  the `op` enum docs. Mitigate with a tight one-line gloss per op and cross-links
  in the group description (e.g. "rect vs box: box is an isometric shaded form").
- **Loud validation is mandatory** — an op that silently ignores a foreign param
  is the one real footgun. Route every dispatcher through the existing
  validate-keys path.

## Optional further step (decide later)

If ~33 still feels heavy, the *reader* side can be grouped into families —
`doc_audit(op = critique|silhouette|components|contrast|palette|ramp)` and
`doc_diff(op = frame|seam|anim|ref|pixel)` — taking it to ~24. The cost is the
output-schema-visibility loss noted above, so this is a deliberate second call,
not part of the safe writer fusion.

## Rollout

Hard break, no aliases (no external users yet — same call as round-2's
removals). Implement one dispatcher at a time behind the existing op machinery,
keeping `cargo test` / clippy green per group; regenerate `TOOLS.md` from the
final surface.
