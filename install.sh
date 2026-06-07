#!/bin/sh
# atelier installer — downloads the latest release binary for this machine.
#
#   curl -fsSL https://raw.githubusercontent.com/marmikshah/atelier/main/install.sh | sh
#
# Options (environment variables):
#   ATELIER_VERSION      install a specific tag (e.g. v1.0.1); default: latest
#   ATELIER_INSTALL_DIR  where the binary goes; default: ~/.local/bin
set -eu

REPO="marmikshah/atelier"
INSTALL_DIR="${ATELIER_INSTALL_DIR:-$HOME/.local/bin}"

say()  { printf '%s\n' "$*"; }
fail() { printf 'install: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar  >/dev/null 2>&1 || fail "tar is required"

# -- pick the release target for this machine --------------------------------
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

# -- resolve the version (the /releases/latest redirect carries the tag) ------
VERSION="${ATELIER_VERSION:-}"
if [ -z "$VERSION" ]; then
  VERSION="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/$REPO/releases/latest" | sed 's|.*/tag/||')"
  [ -n "$VERSION" ] || fail "could not resolve the latest release tag"
fi

URL="https://github.com/$REPO/releases/download/$VERSION/atelier-$VERSION-$TARGET.tar.gz"

# -- download, extract, install ----------------------------------------------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say "Downloading atelier $VERSION ($TARGET)..."
curl -fsSL "$URL" -o "$TMP/atelier.tar.gz" \
  || fail "download failed: $URL"
tar -xzf "$TMP/atelier.tar.gz" -C "$TMP"

mkdir -p "$INSTALL_DIR"
install -m 755 "$TMP/atelier" "$INSTALL_DIR/atelier"

say "Installed: $INSTALL_DIR/atelier ($("$INSTALL_DIR/atelier" --version))"

# -- PATH hint -----------------------------------------------------------------
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say ""
     say "Note: $INSTALL_DIR is not on your PATH. Add it, e.g.:"
     say "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

BIN="$INSTALL_DIR/atelier"

# -- offer registration with detected MCP clients ------------------------------
# Interactive prompts read /dev/tty (stdin is the pipe under `curl | sh`).
# Non-interactive runs skip prompts; ATELIER_REGISTER="all" (or a comma list of
# claude,kimi,cursor) pre-answers yes for scripted setups.
ask_yn() { # ask_yn <tool-key> <question>  -> 0 = yes
  case ",${ATELIER_REGISTER:-}," in
    *,all,*|*",$1,"*) return 0 ;;
  esac
  { true < /dev/tty; } 2>/dev/null || return 1 # no usable terminal -> skip
  printf '%s [Y/n] ' "$2" > /dev/tty
  read -r ans < /dev/tty || return 1
  case "$ans" in n|N|no|NO) return 1 ;; *) return 0 ;; esac
}

# Claude Code and Kimi Code share the same `mcp add` CLI shape.
register_cli() { # register_cli <binary> <display name>
  command -v "$1" >/dev/null 2>&1 || return 0
  if ask_yn "$1" "Register atelier with $2?"; then
    if "$1" mcp add --scope user atelier -- "$BIN" >/dev/null 2>&1; then
      say "Registered with $2 (restart your session to load the tools)."
    else
      say "Could not register automatically — run manually:"
      say "  $1 mcp add --scope user atelier -- $BIN"
    fi
  fi
}

register_cursor() {
  { command -v cursor >/dev/null 2>&1 || [ -d "$HOME/.cursor" ]; } || return 0
  CFG="$HOME/.cursor/mcp.json"
  ask_yn cursor "Register atelier with Cursor?" || return 0
  if [ ! -f "$CFG" ]; then
    mkdir -p "$HOME/.cursor"
    printf '{\n  "mcpServers": {\n    "atelier": { "command": "%s" }\n  }\n}\n' "$BIN" > "$CFG"
    say "Registered with Cursor ($CFG)."
  elif command -v jq >/dev/null 2>&1; then
    jq --arg bin "$BIN" '.mcpServers.atelier = {command: $bin}' "$CFG" > "$CFG.tmp" \
      && mv "$CFG.tmp" "$CFG"
    say "Registered with Cursor ($CFG)."
  else
    say "Cursor config exists and jq is unavailable — add this to $CFG manually:"
    say "  \"atelier\": { \"command\": \"$BIN\" }"
  fi
}

say ""
register_cli claude "Claude Code"
register_cli kimi "Kimi Code"
register_cursor

say ""
say "Manual registration, any MCP client:"
say "  claude mcp add --scope user atelier -- $BIN     # Claude Code / Kimi Code: same shape"
say "  Cursor: ~/.cursor/mcp.json -> \"atelier\": { \"command\": \"$BIN\" }"
