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
#   ATELIER_REGISTER     pre-answer registration: "all" or csv of claude,kimi,cursor
set -eu

REPO="marmikshah/atelier"
INSTALL_DIR="${ATELIER_INSTALL_DIR:-$HOME/.local/bin}"
BIN="$INSTALL_DIR/atelier"
MCP_URL="http://127.0.0.1:8765/mcp"

say()  { printf '%s\n' "$*"; }
fail() { printf 'install: %s\n' "$*" >&2; exit 1; }

# Interactive prompts read /dev/tty (stdin is the pipe under `curl | sh`);
# without a usable terminal every prompt falls back to its default.
has_tty() { { true < /dev/tty; } 2>/dev/null; }
ask() { # ask <question> -> stdout: the answer ("" when no terminal)
  has_tty || { printf ''; return; }
  printf '%s ' "$1" > /dev/tty
  read -r ans < /dev/tty || ans=""
  printf '%s' "$ans"
}

# -- uninstall ------------------------------------------------------------------
# Deregistration is best-effort everywhere: removing an MCP entry that was
# never registered is harmless, so every step tries and continues.
deregister_cli() { # deregister_cli <binary> <display name>
  command -v "$1" >/dev/null 2>&1 || return 0
  "$1" mcp remove --scope user atelier >/dev/null 2>&1 \
    || "$1" mcp remove atelier >/dev/null 2>&1 \
    || return 0
  say "Deregistered from $2."
}

deregister_cursor() {
  CFG="$HOME/.cursor/mcp.json"
  [ -f "$CFG" ] || return 0
  if command -v jq >/dev/null 2>&1; then
    jq 'del(.mcpServers.atelier)' "$CFG" > "$CFG.tmp" 2>/dev/null \
      && mv "$CFG.tmp" "$CFG" \
      && say "Deregistered from Cursor ($CFG)." \
      || rm -f "$CFG.tmp"
  else
    say "Cursor: jq unavailable — remove the \"atelier\" entry from $CFG manually."
  fi
}

do_uninstall() {
  [ -x "$BIN" ] || fail "nothing to uninstall at $BIN"
  "$BIN" service uninstall >/dev/null 2>&1 || true # stop the daemon if present
  rm -f "$BIN"
  deregister_cli claude "Claude Code"
  deregister_cli kimi "Kimi Code"
  deregister_cursor
  say "Removed $BIN (daemon stopped and clients deregistered where present)."
  say "Documents in ~/.atelier are untouched."
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

if [ "$MODE" = "http" ]; then
  if "$BIN" service install; then
    say "Daemon running at $MCP_URL"
  else
    say "Daemon install failed — falling back to stdio registration."
    MODE=stdio
  fi
fi

# -- offer registration with detected MCP clients ---------------------------------
ask_yn() { # ask_yn <tool-key> <question> -> 0 = yes
  case ",${ATELIER_REGISTER:-}," in
    *,all,*|*",$1,"*) return 0 ;;
  esac
  case "$(ask "$2 [Y/n]")" in
    n|N|no|NO) return 1 ;;
    "") has_tty ;; # no terminal -> skip
    *) return 0 ;;
  esac
}

# Claude Code and Kimi Code share the same `mcp add` CLI shape.
register_cli() { # register_cli <binary> <display name>
  command -v "$1" >/dev/null 2>&1 || return 0
  if ask_yn "$1" "Register atelier with $2?"; then
    if [ "$MODE" = "http" ]; then
      ok="$("$1" mcp add --scope user --transport http atelier "$MCP_URL" >/dev/null 2>&1 && echo y || echo n)"
      manual="$1 mcp add --scope user --transport http atelier $MCP_URL"
    else
      ok="$("$1" mcp add --scope user atelier -- "$BIN" >/dev/null 2>&1 && echo y || echo n)"
      manual="$1 mcp add --scope user atelier -- $BIN"
    fi
    if [ "$ok" = "y" ]; then
      say "Registered with $2 (restart your session to load the tools)."
    else
      say "Could not register automatically — run manually:"
      say "  $manual"
    fi
  fi
}

register_cursor() {
  { command -v cursor >/dev/null 2>&1 || [ -d "$HOME/.cursor" ]; } || return 0
  CFG="$HOME/.cursor/mcp.json"
  ask_yn cursor "Register atelier with Cursor?" || return 0
  if [ "$MODE" = "http" ]; then
    ENTRY="{ \"url\": \"$MCP_URL\" }"
    JQ_ENTRY='{url: $v}'; JQ_VAL="$MCP_URL"
  else
    ENTRY="{ \"command\": \"$BIN\" }"
    JQ_ENTRY='{command: $v}'; JQ_VAL="$BIN"
  fi
  if [ ! -f "$CFG" ]; then
    mkdir -p "$HOME/.cursor"
    printf '{\n  "mcpServers": {\n    "atelier": %s\n  }\n}\n' "$ENTRY" > "$CFG"
    say "Registered with Cursor ($CFG)."
  elif command -v jq >/dev/null 2>&1; then
    jq --arg v "$JQ_VAL" ".mcpServers.atelier = $JQ_ENTRY" "$CFG" > "$CFG.tmp" \
      && mv "$CFG.tmp" "$CFG"
    say "Registered with Cursor ($CFG)."
  else
    say "Cursor config exists and jq is unavailable — add this to $CFG manually:"
    say "  \"atelier\": $ENTRY"
  fi
}

say ""
register_cli claude "Claude Code"
register_cli kimi "Kimi Code"
register_cursor

say ""
say "Manual registration, any MCP client:"
if [ "$MODE" = "http" ]; then
  say "  claude mcp add --scope user --transport http atelier $MCP_URL   # Claude Code / Kimi Code"
  say "  Cursor: ~/.cursor/mcp.json -> \"atelier\": { \"url\": \"$MCP_URL\" }"
else
  say "  claude mcp add --scope user atelier -- $BIN     # Claude Code / Kimi Code: same shape"
  say "  Cursor: ~/.cursor/mcp.json -> \"atelier\": { \"command\": \"$BIN\" }"
fi
