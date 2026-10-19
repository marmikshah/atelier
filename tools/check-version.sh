#!/usr/bin/env bash
# Check that the workspace, server.json, its packages, and the npm package name
# one version.
set -euo pipefail
if [[ ${1:-} == --help ]]; then
  echo "Usage: tools/check-version.sh [VERSION]"
  echo "Require Cargo.toml, server.json, its npm and image packages, and"
  echo "npm/package.json to agree, and to equal VERSION when one is given."
  echo "Requires jq."
  exit 0
fi
if [[ $# -gt 1 ]]; then
  echo "Usage: tools/check-version.sh [VERSION] | --help" >&2
  exit 2
fi
cd "$(dirname "${BASH_SOURCE[0]}")/.."
cargo_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
server_version=$(jq -r '.version' server.json)
npm_entry=$(jq -r '.packages[] | select(.registryType == "npm") | .version' server.json)
image=$(jq -r '.packages[] | select(.registryType == "oci") | .identifier' server.json)
npm_version=$(jq -r '.version' npm/package.json)
expected=${1:-$cargo_version}
status=0
check() {
  if [[ $2 != "$expected" ]]; then
    echo "$1 is '$2', expected '$expected'" >&2
    status=1
  fi
}
check "Cargo.toml workspace version" "$cargo_version"
check "server.json version" "$server_version"
check "server.json npm package version" "$npm_entry"
check "server.json image tag" "${image##*:}"
check "npm/package.json version" "$npm_version"
exit "$status"
