# Atelier Learning System — Implementation Plan (v2)

> Status: **public, in-repo**. `atelier-lab` lives at `crates/atelier-lab/` and is pushed with
> the repo (the local-only constraint was dropped). It consumes atelier through its public API
> only and carries `publish = false` — it ships with the source tree, not to crates.io. If the
> system proves out, it becomes a SaaS product; the architecture below is chosen so that move
> is a deployment exercise, not a rewrite.

## Goal

Build a specialized system that:

```text
Text requirement
→ generate several pixel-art drafts
→ rank drafts
→ edit promising candidates through Atelier
→ evaluate each change
→ return the best editable artwork
```

Initial constraints:

```yaml
canvas: 32x32
asset: static, single-subject sprite
frames: 1
background: transparent
palette: maximum 16 colors
categories:
  - characters
  - creatures
  - items
  - props
```

Avoid animation, scenes, large canvases, and arbitrary styles until the static-sprite system works.

---

# Feasibility findings (verified against the codebase)

The plan's architectural assumptions hold up in code. What already exists:

- **In-process API.** `atelier-studio::Studio` is a full synchronous library API — one
  `Result<serde_json::Value, String>` method per editor op. `Atelier::dispatch`
  (`crates/atelier-mcp/src/server/mod.rs:384`) is the single choke point every caller (MCP,
  CLI, replay) funnels through. Embeddable via
  `Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(dir))))`.
- **Isolation.** `Studio::with_docs_dir(path)` roots a studio at an explicit directory without
  touching process-global env — built for exactly this embedding case.
- **Determinism.** `atelier-core` is pure, sync, no timestamps in `doc.json`; scatter/noise
  seeds default to 0; dither is a pure function of (x, y); `atelier replay`
  (`crates/atelier/src/replay.rs`) already rebuilds documents from journals.
- **Structured observations.** `doc_dump_region`, `doc_silhouette`, `doc_components`,
  `doc_palette_report`, `doc_critique`, renders — all plain `Studio` methods returning JSON,
  not MCP-only.
- **PaintPatch compile target.** `doc_paint_grid` already accepts palette-index legends.
- **Recording choke point.** `dispatch` journals every mutation to per-doc `recipe.jsonl`
  (`crates/atelier-mcp/src/recipe.rs` is a shared library format).

Known gaps (small, none architectural):

| Gap | Resolution |
|---|---|
| No indexed-raster read (cels are RGBA PNGs) | Upstream a small `indexed_raster()` read into atelier |
| Journal records requests only — not results or reads | Episode recorder wraps `dispatch`; journal is not reused verbatim |
| Pixel-equality replay claimed but not test-enforced | Phase 2 builds the exact-pixel gate itself; also upstream as an atelier test |
| Checkpoints are disk directory copies + reload per op | Fine at 32×32 (few KB/cel); profile before search; if hot, upstream an in-memory `Document::clone()` checkpoint |
| Clipboard/selection are per-`Studio`, not per-doc | One `Studio` per episode/branch; DSL excludes select/clipboard ops |

---

# Architecture decisions

## A1. Repo placement — same repo, public

- `atelier-lab` lives at `crates/atelier-lab/` as a workspace member with `publish = false`,
  depending on atelier crates by workspace path deps. It is part of the public tree.
- SaaS migration path: move the crate to a private repo, switch path deps to git/vendor.
  Atelier is MIT — no licensing friction.

## A2. Hard boundary — lab never forks atelier internals

- The lab consumes atelier only through `Studio` + `dispatch` + `recipe`.
- Anything atelier-side the lab needs (indexed raster read, in-memory checkpoint,
  pixel-equality replay test) is **upstreamed into atelier as a clean, generally-useful
  addition** — never a `#[cfg]` or feature flag carrying lab-specific behavior.

## A3. SaaS-shaped choices that cost nothing now

- **Per-episode `Studio` + `with_docs_dir`** — this *is* the per-tenant isolation model.
  A future request handler creating an isolated store per session is the same code.
- **Storage trait in front of artifact storage** — local-FS impl now, S3 impl later, without
  touching episode code.
- **Structured JSON observations/episodes, versioned from day one** — these become API
  response bodies unchanged. Every record carries a `format_version`.
- **Recorder as an append-only event log** — future audit trail, usage metering, and billing
  substrate. Include opaque `session_id`/`subject_id` fields even while local.
- **`dispatch` is already async (tokio)** — the env wrapper can sit inside an HTTP handler
  later with zero reshaping.
- **Dataset export format (JSONL + artifact hashes) is a frozen contract** — the trainer
  consumes exports, never atelier crates. The GPU box and the future SaaS box stay decoupled.

## A4. What to avoid

- Hardcoding local paths or `~/.atelier` anywhere outside one config module.
- Letting the trainer import atelier crates.

---

# Phase 1 — Define the experiment

## 1. Create a project specification

Write down:

- What assets are supported
- Canvas dimensions
- Maximum palette size
- Allowed Atelier operations
- Maximum generation attempts
- Maximum edit steps
- Hard output requirements
- Human evaluation criteria

## 2. Define the primary success metric

Use blinded pairwise preference:

> Given the same requirement, is your system's output preferred over an existing model using Atelier?

Track:

- Overall pairwise win rate
- Requirement adherence
- Native-resolution readability
- Silhouette quality
- Palette quality
- Pixel-cluster quality
- Personality and appeal
- Invalid action rate
- Number of Atelier operations
- Generation time

## 3. Create three benchmark splits

```text
development:  40 prompts
validation:   30 prompts
frozen_test:  100 prompts
```

Do not train on the frozen-test prompts.

## 4. Establish baseline outputs

For every development and validation prompt:

- Generate output using your current best model workflow
- Use identical canvas and palette constraints
- Save the final image
- Save the Atelier document
- Save the recipe
- Save the complete model/tool conversation
- Record the number of tool calls and looks

---

# Phase 2 — Add the research environment

## 5. Create the `atelier-lab` crate (workspace member, local only)

```text
crates/atelier-lab/
├── src/
│   ├── env.rs
│   ├── task.rs
│   ├── action.rs
│   ├── observation.rs
│   ├── transition.rs
│   ├── episode.rs
│   ├── recorder.rs
│   ├── replay.rs
│   ├── artifacts.rs
│   ├── corruption.rs
│   ├── evaluation.rs
│   ├── search.rs
│   └── storage.rs        # storage trait; local-FS impl first
└── Cargo.toml
```

Do not put experimental training behavior into `atelier-core` — or anywhere in the public
atelier repo.

## 6. Build an in-process environment wrapper

Expose:

```rust
trait PixelArtEnv {
    fn reset(&mut self, task: &Task) -> Result<Observation>;
    fn observe(&mut self, level: ObservationLevel) -> Result<Observation>;
    fn step(&mut self, action: &Action) -> Result<Transition>;
    fn checkpoint(&mut self) -> Result<CheckpointId>;
    fn restore(&mut self, id: &CheckpointId) -> Result<Observation>;
    fn finish(&mut self) -> Result<EpisodeResult>;
}
```

Back it with an embedded `Atelier::with_studio(...)` (keeps journaling/recording for free)
behind a tokio runtime — not CLI subprocesses, not MCP stdio.

## 7. Isolate environment instances

Give every episode and candidate branch:

- A unique document ID
- A unique workspace (`Studio::with_docs_dir` per episode)
- An isolated Atelier home
- A deterministic random seed
- A separate artifact directory

This lets multiple candidates run without modifying each other — and is the future per-tenant
isolation model.

## 8. Define the task record

Each task should contain:

```json
{
  "id": "character-001",
  "prompt": "A tired knight carrying a chipped red shield",
  "category": "character",
  "width": 32,
  "height": 32,
  "max_colors": 16,
  "must_include": ["knight", "red shield", "visible damage"],
  "must_avoid": [],
  "style": {
    "outline": "selective",
    "lighting": "upper-left",
    "detail": "medium"
  },
  "split": "development"
}
```

## 9. Define two observation levels

### Light observation (after every action)

- Indexed raster (via the upstreamed `indexed_raster()` read)
- Palette
- Layers
- Current stage
- Recent actions
- Basic integrity checks

### Full observation (at candidate-selection points)

- Native-size render
- Nearest-neighbor enlarged render
- Grayscale/value render
- Silhouette/notan render
- Palette report
- Connected components
- Atelier critique
- Document metadata

Do not run every expensive audit after every small edit.

## 10. Build content-addressed artifact storage

Store images and binary states by SHA-256, behind the storage trait:

```text
research/artifacts/sha256/ab/abcdef...
```

Episode JSON references artifact hashes instead of embedding image bytes.

## 11. Record complete episodes

Wrap `dispatch` (the journal alone is insufficient — it records requests only, no results,
no reads). Record:

```text
task
→ observation
→ model reasoning or intent
→ action
→ compiled Atelier calls
→ tool results
→ resulting observation
→ accepted/rejected decision
→ human or critic feedback
```

Record rejected edits as well as accepted edits. Every record carries `format_version`,
`session_id`, and a monotonic sequence number.

## 12. Implement deterministic replay

Given an episode:

- Create a fresh environment
- Replay all accepted actions
- Export the final image
- Compare it to the original artifact
- **Verify exact pixel equality** (this check does not exist in atelier today — build it
  here, and upstream a replay-pixels-match test into atelier)

Do not proceed to model training until replay is reliable.

---

# Phase 3 — Define the learned action language

## 13. Create a compact action DSL

Start with approximately ten actions:

```rust
enum Action {
    PaintPatch,
    ClearRegion,
    MoveRegion,
    MirrorRegion,
    ReplaceColor,
    SetPalette,
    AddLayer,
    MergeLayer,
    AdvanceStage,
    Finish,
}
```

Do not initially train against every raw MCP tool. The DSL deliberately excludes
select/clipboard ops (they are per-`Studio` shared state).

## 14. Make `PaintPatch` the primary raster action

Represent it as:

```text
layer
x
y
width
height
palette-index grid
```

Compile it into Atelier's `doc_paint_grid` (which already accepts palette-index legends).

## 15. Add artistic stage state

```text
Specification
Silhouette
ColorBlocking
Lighting
Detail
Cleanup
Finished
```

Every transition records the current stage.

## 16. Implement the action compiler

The compiler must:

- Convert DSL actions into Atelier operations
- Check coordinate bounds
- Check palette indices
- Check layer/frame existence
- Enforce patch-size limits
- Reject empty modifications
- Enforce stage-specific restrictions
- Return structured errors

## 17. Add action-effect metadata

Every model-proposed action should include:

```json
{
  "action": {},
  "intent": "Separate the shield from the torso",
  "target_region": [4, 10, 12, 18],
  "preserve": ["helmet silhouette", "shield damage"],
  "expected_effect": "Improve silhouette readability"
}
```

This will later help edit evaluation and training.

---

# Phase 4 — Build the human evaluation system

## 18. Implement a pairwise annotation UI

Show:

- Requirement
- Candidate A at native size
- Candidate B at native size
- Both candidates at 8× nearest-neighbor scale
- Optional grayscale and silhouette views

Ask:

1. Which is better overall?
2. Which follows the prompt better?
3. Which reads better at native resolution?
4. Why?

(Local web page is fine now; the same comparison payload is the future crowd-annotation API.)

## 19. Use fixed reason labels

```text
requirement adherence
silhouette
composition
pose
proportions
palette
lighting
pixel clusters
readability
personality
style
polish
```

Allow an optional free-text explanation.

## 20. Prevent annotation bias

- Randomize A/B placement
- Hide model identity
- Hide generation time
- Hide tool-call count
- Occasionally repeat comparisons
- Occasionally reverse A/B
- Measure your own consistency

## 21. Label the first dataset

Initial target:

```text
200 comparisons for UI validation
1,000 comparisons for early critic development
2,000–5,000 comparisons for the first useful critic
```

Include:

- Different outputs for the same prompt
- Parent-versus-child edits
- Model output versus your correction
- Good art versus subtle corruptions
- Pairs that are genuinely close

---

# Phase 5 — Build synthetic corruptions

## 22. Implement five basic corruptions first

1. Isolated pixel insertion
2. Palette bloat
3. Broken outline
4. Silhouette collision
5. Reduced value contrast

## 23. Expand to fifteen corruption families

Add:

- Banding
- Irregular jaggies
- Pillow shading
- Flattened values
- Mixed light direction
- Excessive texture
- Removed focal highlight
- Broken symmetry
- Removed required feature
- Near-duplicate colors

## 24. Return structured corruption metadata

Every corruption should produce:

```text
clean state
corrupted state
corruption type
affected region
severity
forward operation
possible inverse operation
```

## 25. Generate severity levels

```text
subtle
moderate
severe
```

Do not let the critic learn only obvious defects.

## 26. Generate the first synthetic dataset

Target:

```text
50,000 clean-versus-corrupted pairs
10,000 defect-localization records
5,000 corruption-repair transitions
```

Keep synthetic and human preference data separately identifiable (a `source` field in the
frozen export format).

---

# Phase 6 — Build the critic

## 27. Start with pairwise ranking only

Input:

```text
requirement
candidate A indexed raster
candidate B indexed raster
candidate A render
candidate B render
optional Atelier reports
```

Output:

```text
A preferred
B preferred
tie
confidence
```

Do not begin with free-form critique generation.

## 28. Implement two image representations

### Exact raster representation

For each pixel:

```text
palette index embedding
+ x-position embedding
+ y-position embedding
```

### Perceptual representation

Use a small CNN or ViT over:

- Native render
- Enlarged render
- Grayscale render
- Silhouette render

Fuse the two representations with the requirement text.

## 29. Use a small critic first

Development ladder:

```text
5M–10M: correctness and overfitting
30M–50M: data validation
100M–200M: first serious critic
```

## 30. Train the critic in stages

1. Clean-versus-corrupted ranking
2. Defect classification
3. Defect localization
4. Parent-versus-child ranking
5. Human artistic-preference fine-tuning

## 31. Evaluate critic failure modes

Test whether it incorrectly assumes:

- More colors are always better
- Fewer colors are always better
- More contrast is always better
- More detail is always better
- Later revisions are always better
- Larger foreground objects are always better
- Mechanically cleaner art is always more appealing

## 32. Set a critic acceptance gate

Before using it for autonomous search, require:

```text
≥80% agreement with frozen human comparisons
minimal left/right order bias
stable confidence calibration
good performance on subtle corruptions
good performance on independent model outputs
```

If it fails, collect more hard comparisons instead of increasing model size immediately.

---

# Phase 7 — Build the generator dataset

## 33. Gather high-quality sprite data

For each sample, retain:

- Requirement or caption
- Indexed raster
- Canonical palette
- Canvas dimensions
- Category
- Style attributes
- Source and license
- Quality status

Prioritize data you own or can clearly use.

## 34. Normalize all samples

- Remove anti-aliasing
- Reject rescaled/blurry images
- Preserve transparent backgrounds
- Fit within 32×32
- Reduce to a maximum of 16 colors
- Remove near duplicates
- Keep train and evaluation assets separate

## 35. Canonicalize palettes

Use deterministic ordering:

1. Transparency at index 0
2. Dominant color families
3. Colors ordered by lightness within a family
4. Hue/chroma as tie-breakers

Version the canonicalization algorithm.

## 36. Generate textual descriptions

Prefer:

- Human-written requirements
- Metadata-derived descriptions
- Structured attributes
- Carefully reviewed descriptions

Keep descriptions concise and visually grounded.

---

# Phase 8 — Train the draft generator

## 37. Define generator output

```text
requirement
→ structured specification
→ palette
→ silhouette
→ 32×32 palette-index raster
```

The output should be directly importable into Atelier (via `doc_paint_grid`).

## 38. Train an overfitting prototype

Before a real run:

- Use 10–100 samples
- Confirm exact memorization
- Verify palette reconstruction
- Verify foreground generation
- Verify transparent background handling
- Verify deterministic import into Atelier

## 39. Train the model ladder

```text
20M–50M: pipeline smoke model
100M–200M: objective and data experiments
250M–500M: primary draft generator
```

## 40. Handle background imbalance

Because most pixels may be transparent:

- Weight foreground pixels more heavily
- Track foreground and background loss separately
- Track silhouette IoU
- Reject trivial all-transparent solutions

## 41. Evaluate drafts using best-of-N

For every validation requirement:

1. Sample eight drafts.
2. Reject invalid drafts.
3. Rank drafts manually and with the critic.
4. Measure whether at least one draft is usable.
5. Measure whether the critic selects the human-preferred draft.

The generator is successful when it produces strong starting material, not necessarily finished art.

---

# Phase 9 — Collect editor demonstrations

## 42. Correct generator outputs manually

For weak drafts:

- Open them in Atelier
- Make deliberate corrections
- Record each action
- Record the reason for each meaningful edit
- Mark the action as accepted
- Save intermediate versions

## 43. Create synthetic repair demonstrations

For every corruption:

```text
corrupted state
→ repair action
→ clean or improved state
```

Start with one-action repairs.

## 44. Collect baseline-model editing trajectories

Run existing models against the same tasks.

Keep:

- Successful edits
- Failed edits
- Invalid calls
- Unnecessary edits
- Edits that fix one issue but damage another

## 45. Build the editor dataset

Each sample should contain:

```text
requirement
stage
current raster and palette
current render
audit results
recent actions
target defect
accepted next action
```

For preference records, include:

```text
same state
chosen action
rejected action
resulting child states
preference reason
```

---

# Phase 10 — Train the editor policy

## 46. Train on one-action repairs first

Curriculum:

1. Isolated pixels
2. Broken outlines
3. Palette duplication
4. Weak value separation
5. Silhouette collisions
6. Proportion changes
7. Lighting changes
8. Controlled detail
9. General revision

## 47. Constrain action decoding

The model should only be able to produce:

- Valid action names
- Valid coordinate tokens
- Existing palette indices
- Existing layer IDs
- Bounded patch sizes
- Valid stage transitions

Do not depend on the model to produce correct arbitrary JSON.

## 48. Train the model ladder

```text
20M–50M: action syntax and basic repairs
100M–200M: corruption repair
250M–500M: general editor
```

## 49. Evaluate one-step improvement

For each validation state:

1. Ask the editor for one action.
2. Execute it.
3. Compare parent and child.
4. Get human preference.
5. Record whether the action addressed its stated intent.

Track:

```text
improved
neutral
damaged
invalid
```

Do not move to long episodes until one-step edits improve more often than they damage.

---

# Phase 11 — Implement search

## 50. Add draft search

```text
Generate 8 drafts
→ hard-filter invalid drafts
→ critic ranks them
→ keep top 2
```

## 51. Add one-step action search

For each current state:

1. Save a checkpoint.
2. Sample six editor actions.
3. Restore the checkpoint before every action.
4. Execute each action independently.
5. Hard-filter invalid or damaging children.
6. Rank children with the critic.
7. Keep the best child.
8. Record all alternatives.

(Profile checkpoint/restore cost here first — atelier checkpoints are disk copies; if hot,
upstream the in-memory `Document::clone()` checkpoint per the feasibility table.)

## 52. Add beam search

Starting configuration:

```yaml
drafts: 8
initial_survivors: 2
actions_per_state: 6
beam_width: 2
maximum_depth: 8
```

## 53. Add stage-specific budgets

```text
Silhouette:     2–3 edits
Color blocking: 1–2 edits
Lighting:       1–2 edits
Detail:         1–2 edits
Cleanup:        1–2 edits
```

## 54. Add edit preservation checks

Before accepting a child, compare:

- Required features
- Silhouette
- Palette
- Preserved regions
- Parent and child critic scores
- Target-defect resolution

Always retain the current best checkpoint.

## 55. Define stopping conditions

Stop when:

- The editor emits `Finish`
- Maximum depth is reached
- No candidate improves the score
- Two consecutive iterations fail to improve
- Only low-severity defects remain
- The hard tool budget is exhausted

---

# Phase 12 — Distill search into the editor

## 56. Save search-selected actions

Every search step creates:

```text
state
selected action
rejected actions
child scores
final human preference if available
```

## 57. Retrain the editor on selected actions

Mix:

- Human correction actions
- Synthetic repair actions
- Critic-selected actions
- Human-confirmed search actions

Give highest weight to human-confirmed examples.

## 58. Compare distilled policy against search

Measure:

- Quality without branching
- Number of required candidate actions
- Tool calls
- Invalid actions
- Parent-child improvement rate

The editor should gradually need less search.

---

# Phase 13 — Preference optimization

## 59. Build chosen/rejected action pairs

Use states where:

- Both actions are valid
- The outcomes differ meaningfully
- The preferred result is human-confirmed or high-confidence
- The reason for preference is known

## 60. Preference-tune the editor

Optimize the editor to favor actions that produce preferred child states.

Begin with one-action preferences. Do not start with full-episode RL.

## 61. Test for reward hacking

Look for:

- Removing detail to obtain cleaner metrics
- Avoiding foreground pixels
- Repeatedly increasing contrast
- Repeatedly reducing palette size
- Making no-op actions
- Exploiting critic blind spots
- Overfitting to corruption patterns

---

# Phase 14 — End-to-end evaluation

## 62. Freeze system settings

Before final evaluation, freeze:

- Generator checkpoint
- Critic checkpoint
- Editor checkpoint
- Sampling parameters
- Search width/depth
- Action budget
- Atelier version
- Dataset version

## 63. Run every frozen prompt

For each prompt, produce:

- Frontier baseline
- Generator-only result
- Generator plus greedy editor
- Generator plus searched editor
- Final distilled policy result

## 64. Conduct blinded comparisons

Primary comparison:

```text
Full Atelier specialist
vs.
frontier model with the same Atelier constraints
```

## 65. Publish an internal result report

Include:

- Pairwise win rates
- Per-category results
- Requirement adherence
- Critic agreement
- One-step edit improvement rate
- Search gain
- Tool calls
- Runtime
- H100 training hours
- Common failure cases
- Best and worst examples

---

# Recommended build order

Follow this order strictly:

```text
0. Upstream into atelier: indexed_raster() read, replay pixel-equality test
1. Project specification
2. Frozen benchmark
3. atelier-lab environment (private repo, path deps)
4. Complete trajectory recording (dispatch wrapper, versioned event log)
5. Deterministic replay (with exact-pixel gate)
6. Pairwise annotation UI
7. Synthetic corruption engine
8. Critic dataset
9. Critic model
10. Generator dataset
11. Draft generator
12. Manual correction trajectories
13. Action DSL + compiler
14. Editor policy
15. One-step action search (profile checkpoint cost first)
16. Beam search
17. Search distillation
18. Preference optimization
19. End-to-end comparison
```

---

# First two weeks

## Week 1

- [x] Freeze the 32×32 static-sprite scope
- [ ] Write 40 development prompts
- [x] Create `crates/atelier-lab` as a workspace member (local only, not pushed)
- [x] Upstream `indexed_raster()` into atelier (core method + `Studio::doc_indexed_raster`; MCP tool wiring deferred)
- [x] Implement `Task`
- [x] Implement `Observation`
- [x] Implement `Action`
- [x] Implement `Transition`
- [x] Wrap Atelier dispatch (embedded `Atelier::with_studio`, tokio runtime)
- [x] Create isolated episode environments (`with_docs_dir` per episode)
- [x] Capture native and enlarged renders

## Week 2

- [x] Implement episode recording (dispatch wrapper, versioned event log)
- [x] Implement artifact hashing (storage trait + local-FS impl)
- [x] Implement deterministic replay with exact-pixel verification
- [x] Upstream replay pixel-equality test into atelier
- [ ] Generate four baseline outputs for 20 prompts
- [x] Build the minimal pairwise annotation page
- [ ] Label 200 comparisons
- [x] Implement five corruptions
- [ ] Export the first critic dataset (frozen JSONL + hashes format)
- [ ] Train a tiny critic to overfit the dataset

---

# Milestones

Do not begin the generator until this loop works:

```text
Atelier episode
→ recorded state/action/result
→ generated pairwise dataset
→ trained critic
→ critic ranks two sprites
→ comparison against your judgment
```

1. **A critic that reliably identifies the better of two Atelier-generated sprites.**
2. **A generator whose best-of-eight outputs provide strong usable drafts.**
3. **An editor whose one-step actions improve sprites more often than they damage them.**

Once those three work independently, combine them into the full generation-and-refinement
system — locally first. If it earns it, the SaaS version is the same system behind a storage
impl swap and an HTTP shell around `dispatch`.
