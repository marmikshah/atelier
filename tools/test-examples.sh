#!/bin/sh
# Replay every authored example recipe into a throwaway store, failing on the
# first step that errors. The examples double as integration tests — this is
# what makes that claim true (no unit test drives the real binary end-to-end
# over stdio, and a deleted or renamed tool would otherwise rot them silently).
#
# Runs the debug binary; `make test` builds it first.
set -eu

repo=$(pwd)
bin="$repo/target/debug/atelier"
[ -x "$bin" ] || { echo "test-examples: $bin missing — run 'cargo build' first" >&2; exit 2; }

store=$(mktemp -d)
scratch=$(mktemp -d)
trap 'rm -rf "$store" "$scratch"' EXIT

status=0
for recipe in "$repo"/docs/examples/*.json; do
    echo "== $(basename "$recipe")"
    # Run from the scratch dir so recipes' relative out_paths (exported
    # GIFs/sheets) land there, not in the repo.
    if ! (cd "$scratch" && "$bin" replay "$recipe" --home "$store"); then
        echo "test-examples: $(basename "$recipe") failed" >&2
        status=1
    fi
done
[ "$status" -eq 0 ] && echo "examples: all replayed ok"
exit "$status"
