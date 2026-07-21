# Task packs

Task JSONL files in this directory are reviewed benchmark inputs, not generated
research output. Large episode traces, annotations, and model-produced datasets
belong under the gitignored `research/` directory.

`development.jsonl` freezes the first 40 development tasks: ten each for
characters, creatures, items, and props. Every task uses the v1 32x32 static
sprite scope and a palette budget of at most 16 colors.

Validate a pack before starting model calls:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  validate-tasks crates/atelier-lab/tasks/development.jsonl
```
