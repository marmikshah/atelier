#!/bin/sh
# atelier installer — installs the binary. That's all an agent driving atelier
# from a shell needs: `atelier call ...` works with zero setup. Running as an
# MCP server (background daemon, or a stdio server your client spawns) is an
# optional add-on the script offers at the end.
#
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh -s -- --yes
#   curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh -s -- uninstall
#   ./install.sh --source          # build this branch and install it
#
# Flags:
#   --yes, -y            non-interactive; take every default (no daemon — MCP is opt-in)
#   --source, --build    build the current checkout instead of downloading
#   uninstall            remove the binary and the daemon
#
# Options (environment variables):
#   ATELIER_YES=1        same as --yes (non-interactive)
#   ATELIER_VERSION      install a specific tag (e.g. v1.0.1); default: latest
#   ATELIER_INSTALL_DIR  where the binary goes; default: ~/.local/bin
#   ATELIER_MODE         MCP add-on mode: "http" (background daemon) or "stdio"
#                        (client spawns the server). Unset: ask; default answer: neither.
set -eu

REPO="marmikshah/atelier"
INSTALL_DIR="${ATELIER_INSTALL_DIR:-$HOME/.local/bin}"
BIN="$INSTALL_DIR/atelier"
MCP_URL="http://127.0.0.1:8765/mcp"

# --source / --build builds the current checkout instead of downloading a release.
# --yes / -y (or ATELIER_YES=1) runs fully non-interactively, taking every
# default: reinstall, the background daemon.
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

# -- optional: the MCP server add-on ----------------------------------------------
# The binary already does everything from the CLI (`atelier call ...`) — no
# server, no registration. MCP is for clients that ONLY speak MCP: a shared
# background daemon (launchd / systemd --user) at $MCP_URL, or a stdio server
# the client spawns per session (each gets its own store). Opt in here, or
# later with `atelier install`.
MODE="${ATELIER_MODE:-}"
if [ -z "$MODE" ]; then
  say ""
  case "$(ask "Set up the MCP server add-on? [d]aemon, [s]tdio, or [N]either (default)")" in
    d|D) MODE=http ;;
    s|S) MODE=stdio ;;
    *)   MODE=cli ;;
  esac
fi

if [ "$MODE" = "http" ]; then
  if "$BIN" install; then
    say "Daemon running at $MCP_URL"
  else
    say "Daemon install failed — atelier still works from the CLI (atelier call ...)."
    MODE=cli
  fi
fi

# -- skills (Claude Code, Kimi Code, Cursor) ---------------------------------------
# The workflow guidance that teaches an agent to use atelier well: build in
# layers, look after every pass, fix the region rather than repaint the frame.
# The binary carries them and writes the SKILL.md files itself — no per-file
# download, no path coupling. Optional; atelier works without them.
SKILL_REFRESHED=""
for TARGET in claude kimi cursor; do
  case "$TARGET" in
    claude) SKILL_DIR="$HOME/.claude/skills" ;;
    kimi)   SKILL_DIR="$HOME/.kimi-code/skills" ;;
    cursor) SKILL_DIR="$HOME/.cursor/skills" ;;
  esac
  if [ -d "$SKILL_DIR/atelier-sprite" ]; then
    # Already installed: refresh without asking. They are ours to update.
    "$BIN" skills install --for "$TARGET" >/dev/null 2>&1 \
      && say "Skills updated in $SKILL_DIR" || say "Skills update failed for $TARGET — skipping."
    SKILL_REFRESHED=1
  fi
done

if [ -z "$SKILL_REFRESHED" ]; then
  case "$(ask "Install the atelier skills for your agent? [Y/n]")" in
    n|N) say "Skipped skills. Install later with: atelier skills install --for all" ;;
    *)
      # Claude Code always; Kimi Code and Cursor where their config dir exists.
      TARGETS="claude"
      [ -d "$HOME/.kimi-code" ] && TARGETS="$TARGETS kimi"
      [ -d "$HOME/.cursor" ] && TARGETS="$TARGETS cursor"
      for TARGET in $TARGETS; do
        if "$BIN" skills install --for "$TARGET" >/dev/null 2>&1; then
          say "Skills installed for $TARGET (atelier works without them; they teach the workflow)."
        else
          say "Skills install failed for $TARGET — atelier itself is fine. Retry: atelier skills install --for $TARGET"
        fi
      done
      ;;
  esac
fi

# -- next step ---------------------------------------------------------------------
say ""
if [ "$MODE" = "http" ]; then
  say "Register the MCP daemon with your client (then restart its session):"
  say "  Claude Code: claude mcp add --scope user --transport http atelier $MCP_URL"
  say "  Kimi Code:   ~/.kimi-code/mcp.json -> \"atelier\": { \"url\": \"$MCP_URL\" }"
  say "  Cursor:      ~/.cursor/mcp.json    -> \"atelier\": { \"url\": \"$MCP_URL\" }"
elif [ "$MODE" = "stdio" ]; then
  say "Register the stdio MCP server with your client (then restart its session):"
  say "  Claude Code: claude mcp add --scope user atelier -- $BIN"
  say "  Kimi Code:   ~/.kimi-code/mcp.json -> \"atelier\": { \"command\": \"$BIN\" }"
  say "  Cursor:      ~/.cursor/mcp.json    -> \"atelier\": { \"command\": \"$BIN\" }"
else
  say "Try it — no setup, no registration:"
  say "  atelier call doc_create '{\"name\":\"cat\",\"width\":32,\"height\":32}'"
  say "  atelier call doc_look '{\"doc_id\":\"cat\",\"out_path\":\"/tmp/cat.png\"}'"
  say "  atelier doctor"
  say ""
  say "Any agent with a shell can drive it the same way. For MCP-only clients:"
  say "  atelier install   # background daemon at $MCP_URL (see README for stdio mode)"
fi
