#!/bin/sh
# atelier installer — installs the binary and sets up the background daemon.
# By default it downloads the latest release; --source builds the current
# checkout instead. Registers with your MCP client at the end.
#
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh -s -- --yes
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh -s -- uninstall
#   ./install.sh --source          # build this branch and install it
#
# Flags:
#   --yes, -y            non-interactive; take every default (daemon + defaults)
#   --source, --build    build the current checkout instead of downloading
#   uninstall            remove the binary and the daemon
#
# Options (environment variables):
#   ATELIER_YES=1        same as --yes (non-interactive)
#   ATELIER_VERSION      install a specific tag (e.g. v1.0.1); default: latest
#   ATELIER_INSTALL_DIR  where the binary goes; default: ~/.local/bin
#   ATELIER_MODE         "http" (background daemon, default) or "stdio" (client spawns it)
set -eu

REPO="marmikshah/atelier"
INSTALL_DIR="${ATELIER_INSTALL_DIR:-$HOME/.local/bin}"
BIN="$INSTALL_DIR/atelier"
MCP_URL="http://127.0.0.1:8765/mcp"

# --source / --build builds the current checkout instead of downloading a release.
# --yes / -y (or ATELIER_YES=1) runs fully non-interactively, taking every
# default: reinstall, the background daemon, the default tool profile.
FROM_SOURCE=""
YES="${ATELIER_YES:-}"
for a in "$@"; do case "$a" in
  --source|--build) FROM_SOURCE=1 ;;
  --yes|-y) YES=1 ;;
esac; done

say()  { printf '%s\n' "$*"; }
fail() { printf 'install: %s\n' "$*" >&2; exit 1; }

# Interactive prompts read /dev/tty (stdin is the pipe under `curl | sh`).
# --yes, or no usable terminal, makes every prompt fall back to its default.
ask() { # ask <question> -> stdout: the answer ("" when non-interactive)
  [ -n "$YES" ] && { printf ''; return; }
  { true < /dev/tty; } 2>/dev/null || { printf ''; return; }
  printf '%s ' "$1" > /dev/tty
  read -r ans < /dev/tty || ans=""
  printf '%s' "$ans"
}

# -- uninstall ------------------------------------------------------------------
do_uninstall() {
  [ -x "$BIN" ] || fail "nothing to uninstall at $BIN"
  "$BIN" uninstall >/dev/null 2>&1 || true # stop the daemon if present
  rm -f "$BIN"
  say "Removed $BIN (and stopped the background daemon, if one was installed)."
  say "Documents in ~/.atelier are untouched. If registered with an MCP client,"
  say "deregister manually, e.g.: claude mcp remove atelier"
  exit 0
}
[ "${1:-}" = "uninstall" ] && do_uninstall

# -- existing installation ------------------------------------------------------
if [ -x "$BIN" ]; then
  CURRENT="$("$BIN" --version 2>/dev/null || echo "unknown version")"
  case "$(ask "Found $CURRENT at $BIN — [R]einstall or [u]ninstall?")" in
    u|U) do_uninstall ;;
    *)   say "Updating existing installation." ;;
  esac
fi

if [ -n "$FROM_SOURCE" ]; then
  command -v cargo >/dev/null 2>&1 || fail "--source needs cargo (https://rustup.rs)"
  [ -f Cargo.toml ] || fail "--source must run from inside an atelier checkout"
  say "Building atelier from source (this can take a minute)..."
  cargo build --release || fail "cargo build failed"
  mkdir -p "$INSTALL_DIR"
  install -m 755 target/release/atelier "$BIN"
  say "Installed: $BIN ($("$BIN" --version)) — built from source"
else

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar  >/dev/null 2>&1 || fail "tar is required"

# -- pick the release target for this machine -----------------------------------
OS="$(uname -s)" ARCH="$(uname -m)"
case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64) TARGET="aarch64-apple-darwin" ;;
      *) fail "Intel macOS binaries are not published — install with: cargo install --git https://github.com/$REPO" ;;
    esac ;;
  Linux)
    case "$ARCH" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      *) fail "no prebuilt binary for Linux/$ARCH — install with: cargo install --git https://github.com/$REPO" ;;
    esac ;;
  MINGW*|MSYS*|CYGWIN*)
    fail "on Windows, download the .zip from https://github.com/$REPO/releases/latest" ;;
  *) fail "unsupported platform: $OS/$ARCH" ;;
esac

# -- resolve the version (the /releases/latest redirect carries the tag) --------
VERSION="${ATELIER_VERSION:-}"
if [ -z "$VERSION" ]; then
  VERSION="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/$REPO/releases/latest" | sed 's|.*/tag/||')"
  [ -n "$VERSION" ] || fail "could not resolve the latest release tag"
fi

URL="https://github.com/$REPO/releases/download/$VERSION/atelier-$VERSION-$TARGET.tar.gz"

# -- download, extract, install --------------------------------------------------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say "Downloading atelier $VERSION ($TARGET)..."
curl -fsSL "$URL" -o "$TMP/atelier.tar.gz" \
  || fail "download failed: $URL"
tar -xzf "$TMP/atelier.tar.gz" -C "$TMP"

mkdir -p "$INSTALL_DIR"
install -m 755 "$TMP/atelier" "$BIN"

say "Installed: $BIN ($("$BIN" --version))"
fi

# -- PATH hint --------------------------------------------------------------------
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say ""
     say "Note: $INSTALL_DIR is not on your PATH. Add it, e.g.:"
     say "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

# -- choose how atelier runs ------------------------------------------------------
# stdio: each MCP client spawns its own atelier (zero setup).
# http:  one shared background daemon (launchd / systemd --user) at $MCP_URL —
#        all clients and sessions share a document store; survives reboot.
MODE="${ATELIER_MODE:-}"
if [ -z "$MODE" ]; then
  say ""
  # Daemon is the default (Enter, or a non-interactive `curl | sh`); stdio is opt-in.
  case "$(ask "Run mode — [D]aemon (shared background HTTP, default) or [s]tdio (client spawns it)?")" in
    s|S) MODE=stdio ;;
    *)   MODE=http ;;
  esac
fi

if [ "$MODE" = "http" ]; then
  if "$BIN" install; then
    say "Daemon running at $MCP_URL"
  else
    say "Daemon install failed — register over stdio instead."
    MODE=stdio
  fi
fi

# -- next step ---------------------------------------------------------------------
say ""
say "Register with your MCP client (then restart its session):"
if [ "$MODE" = "http" ]; then
  say "  claude mcp add --scope user --transport http atelier $MCP_URL   # Claude Code / Kimi Code"
  say "  Cursor: ~/.cursor/mcp.json -> \"atelier\": { \"url\": \"$MCP_URL\" }"
else
  say "  claude mcp add --scope user atelier -- $BIN     # Claude Code / Kimi Code: same shape"
  say "  Cursor: ~/.cursor/mcp.json -> \"atelier\": { \"command\": \"$BIN\" }"
fi
