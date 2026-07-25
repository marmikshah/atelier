#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
binary=${ATELIER_BIN:-"$repo/target/debug/atelier"}
verify_root=$(mktemp -d "${TMPDIR:-/tmp}/atelier-showcase.XXXXXX")
trap 'rm -rf "$verify_root"' EXIT

count=0
while IFS= read -r recipe; do
  relative=${recipe#"$repo/benchmarks/replays/"}
  model=${relative%%/*}
  task=${relative##*/}
  task=${task%.jsonl}
  home="$verify_root/homes/$model/$task"
  actual="$verify_root/gifs/$model/$task.gif"
  log="$verify_root/atelier.log"
  mkdir -p "$home" "$(dirname "$actual")"

  if ! "$binary" replay "$recipe" --home "$home" > /dev/null 2>"$log"; then
    echo "showcase-check: replay failed for $model/$task" >&2
    cat "$log" >&2
    exit 1
  fi

  document_count=$(find "$home" -mindepth 1 -maxdepth 1 -type d -print | wc -l | tr -d ' ')
  if [[ $document_count -ne 1 ]]; then
    echo "showcase-check: $model/$task created $document_count documents, expected 1" >&2
    exit 1
  fi
  document=$(find "$home" -mindepth 1 -maxdepth 1 -type d -print | head -1)
  doc_id=$(basename "$document")
  export_args=$(printf \
    '{"doc_id":"%s","op":"anim","out_path":"%s","scale":4,"format":"gif"}' \
    "$doc_id" "$actual")
  if ! "$binary" call doc_export "$export_args" --home "$home" > /dev/null 2>"$log"; then
    echo "showcase-check: export failed for $model/$task" >&2
    cat "$log" >&2
    exit 1
  fi

  expected="$repo/site/showcase/$model/$task.gif"
  if ! cmp -s "$actual" "$expected"; then
    echo "showcase-check: replayed GIF differs from $expected" >&2
    exit 1
  fi
  count=$((count + 1))
done < <(find "$repo/benchmarks/replays" -type f -name '*.jsonl' -print | sort)

if [[ $count -ne 60 ]]; then
  echo "showcase-check: verified $count replay files, expected 60" >&2
  exit 1
fi
echo "showcase-check: all $count replays match their committed GIFs"
