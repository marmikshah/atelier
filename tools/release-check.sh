#!/bin/sh
# Validate the immutable inputs to a release before a tag can publish anything.
# Usage: tools/release-check.sh v1.9.0
#        tools/release-check.sh --current
set -eu

ROOT="$(CDPATH='' cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

fail() {
  printf 'release-check: %s\n' "$*" >&2
  exit 1
}

RELEASE_TAG="${1:-${GITHUB_REF_NAME:-}}"
CHECK_MODE="release"
[ -f Cargo.lock ] || fail "Cargo.lock is missing"
if [ "$RELEASE_TAG" = "--current" ]; then
  CHECK_MODE="current"
  current_id="$(cargo pkgid --locked -p atelier)" \
    || fail "cannot resolve the atelier package"
  current_version="${current_id##*#}"
  current_version="${current_version##*@}"
  RELEASE_TAG="v$current_version"
fi
[ -n "$RELEASE_TAG" ] || fail "pass a tag such as v1.9.0"
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

changelog_heading="$(
  grep -E "^## \[$RELEASE_VERSION\] — (Unreleased|[0-9]{4}-[0-9]{2}-[0-9]{2})$" \
    CHANGELOG.md \
    | head -n 1 \
    || true
)"
[ -n "$changelog_heading" ] \
  || fail "CHANGELOG.md has no valid [$RELEASE_VERSION] heading"
if [ "$CHECK_MODE" = "release" ]; then
  printf '%s\n' "$changelog_heading" \
    | grep -Eq "^## \[$RELEASE_VERSION\] — [0-9]{4}-[0-9]{2}-[0-9]{2}$" \
    || fail "CHANGELOG.md [$RELEASE_VERSION] is still Unreleased; replace it with the release date before tagging"
fi

printf 'release-check: %s matches Cargo.lock, all release packages, and CHANGELOG.md\n' \
  "$RELEASE_TAG"
