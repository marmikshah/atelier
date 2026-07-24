#!/bin/sh
# Validate the immutable inputs to a release before a tag can publish anything.
# Usage: tools/release-check.sh v1.8.0
#        tools/release-check.sh --current
set -eu

ROOT="$(CDPATH= cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

fail() {
  printf 'release-check: %s\n' "$*" >&2
  exit 1
}

RELEASE_TAG="${1:-${GITHUB_REF_NAME:-}}"
[ -f Cargo.lock ] || fail "Cargo.lock is missing"
if [ "$RELEASE_TAG" = "--current" ]; then
  current_id="$(cargo pkgid --locked -p atelier)" \
    || fail "cannot resolve the atelier package"
  current_version="${current_id##*#}"
  current_version="${current_version##*@}"
  RELEASE_TAG="v$current_version"
fi
[ -n "$RELEASE_TAG" ] || fail "pass a tag such as v1.8.0"
printf '%s\n' "$RELEASE_TAG" \
  | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
  || fail "tag must be stable SemVer in the form vMAJOR.MINOR.PATCH"

RELEASE_VERSION="${RELEASE_TAG#v}"
cargo metadata --locked --no-deps --format-version 1 >/dev/null \
  || fail "Cargo.lock is stale"

for package in atelier-core atelier-studio atelier-mcp atelier; do
  package_id="$(cargo pkgid --locked -p "$package")" \
    || fail "cannot resolve package '$package'"
  package_version="${package_id##*#}"
  package_version="${package_version##*@}"
  [ "$package_version" = "$RELEASE_VERSION" ] \
    || fail "$package is $package_version, but the tag is $RELEASE_TAG"
done

grep -F "## [$RELEASE_VERSION] — " CHANGELOG.md >/dev/null \
  || fail "CHANGELOG.md has no dated [$RELEASE_VERSION] heading"

printf 'release-check: %s matches Cargo.lock, all release packages, and CHANGELOG.md\n' \
  "$RELEASE_TAG"
