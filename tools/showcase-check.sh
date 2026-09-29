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

if [[ $count -ne 80 ]]; then
  echo "replay-check: verified $count replay files, expected 80" >&2
  exit 1
fi
echo "replay-check: all $count replays match their committed GIFs"
