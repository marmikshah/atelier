# atelier-lab

`atelier-lab` is the unpublished research environment described in
[`lab.md`](../../lab.md). The current runnable loop turns deterministic Atelier
episodes into blinded comparisons and then into model-independent critic data.

## 1. Write and validate tasks

Keep task records in JSONL and commit the benchmark prompts only after their
splits are frozen. The v1 target is 40 development, 30 validation, and 100
frozen-test prompts.

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  validate-tasks research/tasks.jsonl
```

Every task must be a 32x32 single-subject character, creature, item, or prop
with at most 16 colors. Task ids must be unique.

## 2. Generate complete episodes

Run each baseline or search candidate through `AtelierEnv`, including
`Finish`, then call `finish()`. Keep the resulting episode directories under
`research/`; that directory is gitignored because it can contain large model
traces and human data.

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

## 4. Run the smoke gate

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
agreement on frozen human comparisons.
