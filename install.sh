#!/bin/sh
# atelier installer — downloads the latest release binary for this machine.
#
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh -s -- uninstall
#
# Options (environment variables):
#   ATELIER_VERSION      install a specific tag (e.g. v1.0.1); default: latest
#   ATELIER_INSTALL_DIR  where the binary goes; default: ~/.local/bin
#   ATELIER_MODE         "stdio" (default) or "http" (background daemon)
set -eu

REPO="marmikshah/atelier"
INSTALL_DIR="${ATELIER_INSTALL_DIR:-$HOME/.local/bin}"
BIN="$INSTALL_DIR/atelier"
MCP_URL="http://127.0.0.1:8765/mcp"

say()  { printf '%s\n' "$*"; }
fail() { printf 'install: %s\n' "$*" >&2; exit 1; }

# Interactive prompts read /dev/tty (stdin is the pipe under `curl | sh`);
# without a usable terminal every prompt falls back to its default.
ask() { # ask <question> -> stdout: the answer ("" when no terminal)
  { true < /dev/tty; } 2>/dev/null || { printf ''; return; }
  printf '%s ' "$1" > /dev/tty
  read -r ans < /dev/tty || ans=""
  printf '%s' "$ans"
}

# -- uninstall ------------------------------------------------------------------
do_uninstall() {
  [ -x "$BIN" ] || fail "nothing to uninstall at $BIN"
  "$BIN" service uninstall >/dev/null 2>&1 || true # stop the daemon if present
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
  case "$(ask "Run mode — [S]tdio (client spawns it) or [h]ttp (shared background daemon)?")" in
    h|H) MODE=http ;;
    *)   MODE=stdio ;;
  esac
fi

# -- tool profile -----------------------------------------------------------------
# core (20 tools) is the default: everything the sprite / animation / tile /
# game-set loops need, with a small context footprint. full (all 63) adds the
# long tail (extra effects, audits, rigging). MORE TOOLS DO NOT MEAN BETTER ART —
# every tool still runs via `atelier replay` and recipes regardless; the profile
# only changes what's ADVERTISED to the model, and a bigger surface costs context
# and invites wrong-tool picks. Leave it on core unless you know you want the tail.
PROFILE="${ATELIER_PROFILE:-}"
if [ -z "$PROFILE" ]; then
  say ""
  say "Tool profile — core advertises 20 tools, full advertises all 63."
  say "  (More tools ≠ better graphics: the extra surface only costs the model"
  say "   context and risks wrong-tool picks. Every tool still executes either way.)"
  case "$(ask "Advertise the [C]ore profile or the [f]ull surface?")" in
    f|F) PROFILE=full ;;
    *)   PROFILE=core ;;
  esac
fi

if [ "$MODE" = "http" ]; then
  # The daemon reads ATELIER_PROFILE at launch; export it so `service install`
  # bakes it into the launchd/systemd manifest.
  [ "$PROFILE" = "full" ] && export ATELIER_PROFILE=full
  if "$BIN" service install; then
    say "Daemon running at $MCP_URL (profile: $PROFILE)"
  else
    say "Daemon install failed — register over stdio instead."
    MODE=stdio
  fi
fi

# -- next step ---------------------------------------------------------------------
say ""
say "Register with your MCP client (then restart its session):"
if [ "$MODE" = "http" ]; then
  # The profile is baked into the daemon (above); the client just connects.
  say "  claude mcp add --scope user --transport http atelier $MCP_URL   # Claude Code / Kimi Code"
  say "  Cursor: ~/.cursor/mcp.json -> \"atelier\": { \"url\": \"$MCP_URL\" }"
elif [ "$PROFILE" = "full" ]; then
  # stdio: the client spawns atelier, so the profile rides as a spawn-time env var.
  say "  claude mcp add --scope user --env ATELIER_PROFILE=full atelier -- $BIN   # Claude Code / Kimi Code"
  say "  Cursor: ~/.cursor/mcp.json -> \"atelier\": { \"command\": \"$BIN\", \"env\": { \"ATELIER_PROFILE\": \"full\" } }"
else
  say "  claude mcp add --scope user atelier -- $BIN     # Claude Code / Kimi Code: same shape"
  say "  Cursor: ~/.cursor/mcp.json -> \"atelier\": { \"command\": \"$BIN\" }"
fi
