# atelier-lab

`atelier-lab` is the unpublished research environment described in
[`lab.md`](../../lab.md). The current runnable loop turns deterministic Atelier
episodes into blinded comparisons and then into model-independent critic data.

## 1. Write and validate tasks

Keep task records in JSONL and commit the benchmark prompts only after their
splits are frozen. The first balanced 40-task development pack is checked in
at [`tasks/development.jsonl`](tasks/development.jsonl). The remaining v1
target is 30 validation and 100 frozen-test prompts.

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  validate-tasks crates/atelier-lab/tasks/development.jsonl
```

Every task must be a 32x32 single-subject character, creature, item, or prop
with at most 16 colors. Task ids must be unique.

## 2. Generate complete episodes

Run a task through a provider wrapper implementing the
[`policy` command protocol](policy/README.md):

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  run-policy research/tasks.jsonl item-001 research/episodes \
  --policy ./path/to/provider-wrapper \
  --name baseline-model \
  --max-turns 40
```

The bounded runner records every policy request, response, protocol/provider
failure, compiled action, and token count. It closes incomplete episodes on
turn/error limits so failed attempts remain usable research data. Keep episode
directories under `research/`; that directory is gitignored because it can
contain large model traces and human data.

For a baseline sweep, run a fixed task subset as a resumable batch:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  run-batch crates/atelier-lab/tasks/development.jsonl research/episodes \
  --policy ./path/to/provider-wrapper \
  --name baseline-model-v1 \
  --split development \
  --limit 20 \
  --max-turns 40
```

Each closed attempt appends one versioned row to
`research/episodes/batch-results.jsonl`. On rerun, the command verifies the
referenced episode log and skips only exact completed task/policy/seed keys.
Incomplete attempts remain in the dataset and are retried. `--repeats 4`
uses four stable seeds per task; use a distinct `--name` for each different
model or prompt configuration. `--no-resume` deliberately forces new runs.
Run only one batch writer per episode root at a time.

Before using an episode in a comparison, rebuild its accepted actions and
final image in a fresh store:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  replay research/episodes/episode-123 research/replays
```

The command exits unsuccessfully on the first raster or final-image divergence.

Create an explicit pair manifest (`pairs.jsonl`), one object per line:

```json
{"format_version":1,"id":"item-001-baseline-v-search","candidate_a":{"id":"item-001-baseline","episode_dir":"episodes/baseline-item-001","source":"model","generator":"baseline-v1"},"candidate_b":{"id":"item-001-search","episode_dir":"episodes/search-item-001","source":"search","generator":"search-v1"}}
```

Relative episode paths resolve against the manifest. Bundle the pairs:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  bundle research/pairs.jsonl research/review-bundle
```

The bundle contains `comparisons.jsonl` and a deduplicated content-addressed
artifact store. Native, 8x nearest-neighbor, grayscale, and notan views are
derived from the final episode image, so stale intermediate observations can
never enter a comparison.

## 3. Annotate and export

Follow [`annotation/README.md`](annotation/README.md), then canonicalize the
downloaded left/right judgements:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  export-critic \
  research/review-bundle/comparisons.jsonl \
  research/annotations.jsonl \
  research/critic.jsonl
```

The critic export carries canonical `candidate_a`, `candidate_b`, or `tie`
labels. It does not contain annotator ids. Human and future synthetic labels
remain distinguishable through `label_source`.

## 4. Export generator demonstrations

Create an explicit manifest of curated, completed episodes and export visual
state → next-action SFT examples:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  export-generator research/generator-episodes.jsonl research/generator-sft
```

The exporter refuses frozen-test tasks, rejected actions, incomplete episodes,
and actions discarded by checkpoint restore. See
[`training/README.md`](training/README.md) for the manifest contract and the
complete LoRA train/evaluate/serve workflow.

## 5. Run the critic smoke gate

The dependency-free script is only an overfit/data-plumbing check. It trains a
small linear ranker on exact 32x32 luminance and alpha pixels, skips ties, and
fails if it cannot memorize at least 95% of the decisive rows. It refuses any
`frozen_test` row to prevent accidental evaluation leakage.

```sh
python3 crates/atelier-lab/training/critic_smoke.py \
  research/critic.jsonl research/review-bundle/artifacts
```

Passing does not mean the critic generalizes. The next gates remain a held-out
validation split, order-bias checks, subtle corruptions, and at least 80%
agreement on frozen human comparisons. The test suite exercises this entire
plumbing path with two deterministic, distinct episode rasters and requires
the smoke critic to overfit the exported preference.

## 6. Train the custom VLM loop

The checked-in Qwen3.5-4B QLoRA configs train both sides of the loop, with
Qwen3.5-2B low-VRAM fallbacks:

- generator: task + current raster → one typed Atelier action;
- critic: task + two blinded sprites → canonical preference.

Training dependencies, checkpoints, and datasets are optional research assets,
not Rust workspace dependencies. The training guide includes offline dry-run
validation, exact overfit gates, a persistent local generator server, the
provider-neutral policy client, and bidirectional best-of-N critic ranking.
