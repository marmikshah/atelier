#!/usr/bin/env bash
# Run Atelier's non-mutating local and CI checks.
set -euo pipefail
if [[ ${1:-} == --help ]]; then
  echo "Usage: tools/check.sh — check Rust formatting, Clippy, rustdoc, and tests"
  exit 0
fi
if [[ $# -ne 0 ]]; then
  echo "Usage: tools/check.sh [--help]" >&2
  exit 2
fi
cd "$(dirname "${BASH_SOURCE[0]}")/.."
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
cargo test --locked
