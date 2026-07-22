# Training Atelier VLMs

Atelier trains two adapters over the same small vision-language base:

1. The **generator** sees the task and current raster state, then emits exactly
   one typed `PolicyResponse` action.
2. The **critic** sees the task and two blinded sprites, then emits
   `candidate_a`, `candidate_b`, or `tie`.

The checked-in starting configs use
[`Qwen/Qwen3-VL-2B-Instruct`](https://huggingface.co/Qwen/Qwen3-VL-2B-Instruct)
with PEFT LoRA and optional 4-bit QLoRA. The exporter and policy protocol are
model-independent; changing the base model is a config/runtime decision.

The implementation follows the official
[TRL vision-language SFT format](https://huggingface.co/docs/trl/sft_trainer),
[PEFT all-linear LoRA guidance](https://huggingface.co/docs/peft/en/package_reference/lora),
and [Transformers bitsandbytes QLoRA guidance](https://huggingface.co/docs/transformers/quantization/bitsandbytes).

## 1. Curate generator demonstrations

Do not train on every model attempt. Create an explicit manifest containing
only completed episodes whose surviving action sequence you approve:

```json
{"format_version":1,"id":"knight-human-001","episode_dir":"episodes/episode-123","source":"human"}
```

Relative episode paths resolve against the manifest. Export the accepted
actions on the committed checkpoint branch:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  export-generator research/generator-episodes.jsonl research/generator-sft
```

The export writes `generator.jsonl` and content-addressed state PNGs. It:

- refuses frozen-test tasks and incomplete episodes;
- excludes rejected actions and accepted actions later discarded by restore;
- verifies palette integrity and task constraints;
- reconstructs each pre-action raster and tests the final state against
  Atelier's authoritative render.

Human demonstrations should dominate the first generator dataset. Critic- or
search-selected model episodes can be added later with `source: "search"` or
`source: "model"`, but the manifest remains the explicit approval boundary.

## 2. Prepare critic preferences

Use the annotation workflow in the parent README to produce
`research/critic.jsonl` and its bundle artifact directory. Critic rows keep
human and synthetic label provenance separate and refuse frozen-test training.

## 3. Install the optional GPU environment

The Rust workspace and normal CI do not install ML frameworks:

```sh
python3 -m venv research/vlm-venv
research/vlm-venv/bin/pip install -r \
  crates/atelier-lab/training/requirements-vlm.txt
```

The default configs use 4-bit NF4 and bfloat16. Set `quantization` to `none`
and `bf16` to `false` when targeting hardware that does not support those
modes. Keep downloaded weights and checkpoints under `research/`.

## 4. Validate data without a GPU

These commands load no model and make no network calls:

```sh
python3 crates/atelier-lab/training/train_vlm.py \
  generator research/generator-sft/generator.jsonl \
  research/generator-sft/artifacts \
  crates/atelier-lab/training/configs/generator-qwen3-vl-2b-lora.json \
  --dry-run

python3 crates/atelier-lab/training/train_vlm.py \
  critic research/critic.jsonl research/review-bundle/artifacts \
  crates/atelier-lab/training/configs/critic-qwen3-vl-2b-lora.json \
  --dry-run
```

Both dry-runs are exercised by the repository test suite on generated fixture
data. They validate schemas, split safety, artifact hashes, PNGs, messages,
targets, and configs.

## 5. Quick train + validation

Use the one-command runner first. It trains one epoch on at most 32 development
rows, saves to a separate quick checkpoint, and then reports exact-match
accuracy on at most 32 **validation** rows:

```sh
python3 crates/atelier-lab/training/quick_vlm.py \
  generator research/generator-sft/generator.jsonl \
  research/generator-sft/artifacts \
  crates/atelier-lab/training/configs/generator-qwen3-vl-2b-lora.json

python3 crates/atelier-lab/training/quick_vlm.py \
  critic research/critic.jsonl research/review-bundle/artifacts \
  crates/atelier-lab/training/configs/critic-qwen3-vl-2b-lora.json
```

Run either command with `--dry-run` before moving data to the GPU. Override
`--epochs`, `--train-limit`, `--val-limit`, or `--output-dir` when needed. The
default validation threshold is zero so an initial experiment always reports
its metric; use `--expect-val-accuracy 0.8` to make the command fail below the
target quality gate. The command refuses to run without the configured
validation split and never writes over the full-run checkpoint by default.

## 6. Train and prove overfit first

Remove `--dry-run` to train. Before scaling data, train on a tiny curated
subset and require exact output-contract memorization:

```sh
python3 crates/atelier-lab/training/eval_vlm.py \
  generator research/generator-sft/generator.jsonl \
  research/generator-sft/artifacts \
  --adapter research/checkpoints/generator-qwen3-vl-2b-lora \
  --split development \
  --limit 32 --expect-accuracy 0.95

python3 crates/atelier-lab/training/eval_vlm.py \
  critic research/critic.jsonl research/review-bundle/artifacts \
  --adapter research/checkpoints/critic-qwen3-vl-2b-lora \
  --split development \
  --limit 32 --expect-accuracy 0.95
```

This exact-match gate catches broken image ordering, chat templates, target
masking, adapter loading, and JSON decoding. It is not evidence of artistic
generalization. Generalization requires validation tasks and at least 80%
agreement with frozen human comparisons.

## 7. Serve the trained generator

Keep the model resident in one local process:

```sh
python3 crates/atelier-lab/policy/vlm_server.py \
  --adapter research/checkpoints/generator-qwen3-vl-2b-lora
```

The lightweight command client implements the existing policy protocol, so it
works with single episodes and resumable batches:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  run-batch crates/atelier-lab/tasks/development.jsonl research/vlm-episodes \
  --policy python3 \
  --policy-arg crates/atelier-lab/policy/vlm_client.py \
  --name generator-qwen3-vl-2b-lora \
  --split development --limit 20 --repeats 4
```

`ATELIER_VLM_URL` overrides the loopback URL. The server is deliberately local
by default; do not expose an unauthenticated checkpoint server publicly.

## 8. Rank best-of-N drafts

After training the critic, rank any set of candidate PNGs. Every pair is
evaluated in both A/B orders to make order bias visible:

```sh
python3 crates/atelier-lab/training/rank_candidates.py \
  draft-1.png draft-2.png draft-3.png \
  --tasks crates/atelier-lab/tasks/development.jsonl \
  --task-id development-character-001 \
  --adapter research/checkpoints/critic-qwen3-vl-2b-lora
```

The output contains the ranking, scores, and both directional judgements for
every pair. A disagreement between forward and reverse order is retained, not
silently resolved.
