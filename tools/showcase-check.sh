#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
binary=${ATELIER_BIN:-"$repo/target/debug/atelier"}
verify_root=$(mktemp -d "${TMPDIR:-/tmp}/atelier-showcase.XXXXXX")
trap 'rm -rf "$verify_root"' EXIT

if [[ ! -x $binary ]]; then
  echo "replay-check: missing Atelier binary at $binary" >&2
  exit 1
fi

expected_count=$(python3 - "$repo" <<'PY'
import json
from pathlib import Path
import sys

root = Path(sys.argv[1]) / "showcase"
data = json.loads((root / "runs.json").read_text())
models, tasks = data["models"], data["tasks"]
if not models or not tasks or len(set(models)) != len(models) or len(set(tasks)) != len(tasks):
    sys.exit("replay-check: expected unique, non-empty models and tasks")
expected = {(model, task) for model in models for task in tasks}
runs = [(run["model"], run["task"]) for run in data["runs"]]
if len(runs) != len(expected) or set(runs) != expected:
    sys.exit("replay-check: runs.json does not contain exactly the declared model/task matrix")
for run in data["runs"]:
    stem = f'{run["model"]}/{run["task"]}'
    if run["replay"] != f"replays/{stem}.jsonl" or run["gif"] != f"gifs/{stem}.gif":
        sys.exit(f"replay-check: incorrect artifact paths for {stem}")
for directory, extension in [("replays", "jsonl"), ("gifs", "gif")]:
    paths = {str(path.relative_to(root / directory))
             for path in (root / directory).rglob(f"*.{extension}")}
    required = {f"{model}/{task}.{extension}" for model, task in expected}
    if paths != required:
        sys.exit(f"replay-check: {directory} differs from the declared model/task matrix")
print(len(expected))
PY
)

count=0
while IFS= read -r recipe; do
  relative=${recipe#"$repo/showcase/replays/"}
  model=${relative%%/*}
  task=${relative##*/}
  task=${task%.jsonl}
  home="$verify_root/homes/$model/$task"
  actual="$verify_root/gifs/$model/$task.gif"
  log="$verify_root/atelier.log"
  mkdir -p "$home" "$(dirname "$actual")"

  if ! "$binary" replay "$recipe" --home "$home" > /dev/null 2>"$log"; then
    echo "replay-check: replay failed for $model/$task" >&2
    cat "$log" >&2
    exit 1
  fi

  # Atelier 1.9.0 nests the store under $home/documents/<uuid>; dot-prefixed
  # entries (.transactions) are store internals, not documents.
  docs_root="$home/documents"
  document_count=$(find "$docs_root" -mindepth 1 -maxdepth 1 -type d -not -name '.*' -print | wc -l | tr -d ' ')
  if [[ $document_count -ne 1 ]]; then
    echo "replay-check: $model/$task created $document_count documents, expected 1" >&2
    exit 1
  fi
  document=$(find "$docs_root" -mindepth 1 -maxdepth 1 -type d -not -name '.*' -print | head -1)
  doc_id=$(basename "$document")
  export_args=$(printf \
    '{"doc_id":"%s","op":"anim","out_path":"%s","scale":4,"format":"gif"}' \
    "$doc_id" "$actual")
  if ! "$binary" call doc_export "$export_args" --home "$home" > /dev/null 2>"$log"; then
    echo "replay-check: export failed for $model/$task" >&2
    cat "$log" >&2
    exit 1
  fi

  expected="$repo/showcase/gifs/$model/$task.gif"
  if ! cmp -s "$actual" "$expected"; then
    echo "replay-check: replayed GIF differs from $expected" >&2
    exit 1
  fi
  count=$((count + 1))
done < <(find "$repo/showcase/replays" -type f -name '*.jsonl' -print | sort)

if [[ $count -ne $expected_count ]]; then
  echo "replay-check: verified $count replay files, expected $expected_count" >&2
  exit 1
fi
echo "replay-check: all $count replays match their committed GIFs"
