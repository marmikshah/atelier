#!/bin/sh
# atelier binary installer
#
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh -s -- uninstall
#   ./site/install.sh --source
#
# The script only installs or removes the binary. Daemon setup, MCP client
# registration, tool approvals, and agent skills are explicit `atelier`
# commands printed after installation.
#
# Environment:
#   ATELIER_VERSION      release tag to install (for example v1.8.0)
#   ATELIER_INSTALL_DIR  binary directory (default: ~/.local/bin)
set -eu

REPO="marmikshah/atelier"
INSTALL_DIR="${ATELIER_INSTALL_DIR:-$HOME/.local/bin}"
BIN="$INSTALL_DIR/atelier"
FROM_SOURCE=""
COMMAND="install"

say()  { printf '%s\n' "$*"; }
fail() { printf 'install: %s\n' "$*" >&2; exit 1; }

for argument in "$@"; do
  case "$argument" in
    --source) FROM_SOURCE=1 ;;
    uninstall) COMMAND="uninstall" ;;
    *) fail "unknown argument '$argument'" ;;
  esac
done

if [ "$COMMAND" = "uninstall" ]; then
  [ -e "$BIN" ] || fail "nothing to uninstall at $BIN"
  if [ -x "$BIN" ]; then
    "$BIN" uninstall >/dev/null 2>&1 || true
  fi
  rm -f "$BIN"
  say "Removed $BIN and stopped its background daemon, if installed."
  say "Documents in ~/.atelier and MCP client registrations were left untouched."
  exit 0
fi

if [ -x "$BIN" ]; then
  say "Updating $("$BIN" --version 2>/dev/null || printf 'the existing atelier installation') at $BIN."
fi

if [ -n "$FROM_SOURCE" ]; then
  command -v cargo >/dev/null 2>&1 || fail "--source needs cargo (https://rustup.rs)"
  [ -f Cargo.toml ] || fail "--source must run from inside an atelier checkout"
  say "Building atelier from source..."
  cargo build --release --locked -p atelier || fail "cargo build failed"
  mkdir -p "$INSTALL_DIR"
  install -m 755 target/release/atelier "$BIN"
  say "Installed: $BIN ($("$BIN" --version)) — built from source"
else
  command -v curl >/dev/null 2>&1 || fail "curl is required"
  command -v tar >/dev/null 2>&1 || fail "tar is required"

  OS="$(uname -s)"
  ARCH="$(uname -m)"
  case "$OS" in
    Darwin)
      case "$ARCH" in
        arm64) TARGET="aarch64-apple-darwin" ;;
        *)
          fail "Intel macOS binaries are not published — use: cargo install --locked --git https://github.com/$REPO --package atelier"
          ;;
      esac
      ;;
    Linux)
      case "$ARCH" in
        x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
        *)
          fail "no binary for Linux/$ARCH — use: cargo install --locked --git https://github.com/$REPO --package atelier"
          ;;
      esac
      ;;
    MINGW*|MSYS*|CYGWIN*)
      fail "on Windows, download the .zip from https://github.com/$REPO/releases/latest"
      ;;
    *) fail "unsupported platform: $OS/$ARCH" ;;
  esac

  VERSION="${ATELIER_VERSION:-}"
  if [ -z "$VERSION" ]; then
    VERSION="$(
      curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest" |
        sed 's|.*/tag/||'
    )"
    [ -n "$VERSION" ] || fail "could not resolve the latest release tag"
  fi

  printf '%s\n' "$VERSION" |
    grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' ||
    fail "invalid release version '$VERSION' (expected vMAJOR.MINOR.PATCH)"

  URL="https://github.com/$REPO/releases/download/$VERSION/atelier-$VERSION-$TARGET.tar.gz"
  CHECKSUM_URL="$URL.sha256"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  ARCHIVE="$TMP/atelier.tar.gz"
  CHECKSUM="$TMP/atelier.tar.gz.sha256"

  say "Downloading atelier $VERSION ($TARGET)..."
  curl -fsSL "$URL" -o "$ARCHIVE" || fail "download failed: $URL"

  curl -fsSL "$CHECKSUM_URL" -o "$CHECKSUM" ||
    fail "checksum download failed: $CHECKSUM_URL"
  EXPECTED="$(awk 'NR == 1 {print $1}' "$CHECKSUM")"
  case "$EXPECTED" in
    ""|*[!0-9A-Fa-f]*) fail "invalid checksum at $CHECKSUM_URL" ;;
  esac
  [ "${#EXPECTED}" -eq 64 ] || fail "invalid checksum at $CHECKSUM_URL"

  if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "$ARCHIVE" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    ACTUAL="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
  else
    fail "sha256sum or shasum is required to verify $VERSION"
  fi

  EXPECTED="$(printf '%s' "$EXPECTED" | tr 'A-F' 'a-f')"
  ACTUAL="$(printf '%s' "$ACTUAL" | tr 'A-F' 'a-f')"
  [ "$ACTUAL" = "$EXPECTED" ] || fail "SHA-256 mismatch for $URL"
  say "Verified SHA-256: $ACTUAL"

  tar -xzf "$ARCHIVE" -C "$TMP"
  mkdir -p "$INSTALL_DIR"
  install -m 755 "$TMP/atelier" "$BIN"
  say "Installed: $BIN ($("$BIN" --version))"
fi

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    say ""
    say "Note: $INSTALL_DIR is not on PATH. Add:"
    say "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac

say ""
say "Try it:"
say "  atelier call doc_create '{\"name\":\"cat\",\"width\":32,\"height\":32}'"
say ""
say "Optional setup (nothing below runs automatically):"
say "  atelier install   # background MCP daemon; asks for a port"
say "  atelier status    # endpoint and service state"
say "  atelier skills install --for <claude|codex|kimi|cursor|all>"
