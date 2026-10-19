#!/usr/bin/env bash
# Check that the workspace, server.json, and its image tag name one version.
set -euo pipefail
if [[ ${1:-} == --help ]]; then
  echo "Usage: tools/check-version.sh [VERSION]"
  echo "Require Cargo.toml, server.json, and the server.json image tag to agree,"
  echo "and to equal VERSION when one is given. Requires jq."
  exit 0
fi
if [[ $# -gt 1 ]]; then
  echo "Usage: tools/check-version.sh [VERSION] | --help" >&2
  exit 2
fi
cd "$(dirname "${BASH_SOURCE[0]}")/.."
cargo_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
server_version=$(jq -r '.version' server.json)
image=$(jq -r '.packages[] | select(.registryType == "oci") | .identifier' server.json)
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
check "server.json image tag" "${image##*:}"
exit "$status"
