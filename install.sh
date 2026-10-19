#!/bin/sh
# Install atelier from a GitHub Release and connect it to your coding agents.
#
#   curl -fsSL https://github.com/marmikshah/atelier/releases/latest/download/install.sh | sh
#
# On Ubuntu Desktop, which has wget but not curl:
#
#   wget -qO- https://github.com/marmikshah/atelier/releases/latest/download/install.sh | sh
#
# Downloads the binary for this machine, checks it against the release's
# SHA256SUMS, puts it in ~/.local/bin, adds that to PATH in your shell profile
# if it is missing, and runs `atelier connect`, which registers it with
# Claude Code and Codex when they are installed. Run it again to update.
# `atelier uninstall` removes everything it set up.
#
# Environment:
#   ATELIER_VERSION         release to install, such as v0.2.0 (default: latest)
#   ATELIER_INSTALL_DIR     directory for the binary (default: ~/.local/bin)
#   ATELIER_NO_MODIFY_PATH  set to 1 to leave shell profiles alone
#   ATELIER_NO_CONNECT      set to 1 to skip `atelier connect`
#   ATELIER_DOWNLOAD_URL    base URL that holds the release files, for mirrors
#                           and tests
set -eu

repo=marmikshah/atelier
# Ends the line added to a shell profile; `atelier uninstall` removes lines
# that end with it, and the fish snippet file named below.
marker='# added by the atelier installer'

say() {
  printf '%s\n' "$*"
}

fail() {
  printf 'atelier install: %s\n' "$*" >&2
  exit 1
}

download() {
  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https,file' --tlsv1.2 -fsSL -o "$2" "$1" || fail "could not download $1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1" || fail "could not download $1"
  else
    fail "curl or wget is needed to download atelier"
  fi
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    fail "sha256sum or shasum is needed to check the download"
  fi
}

# Show a path under the home directory as ~/...
tilde() {
  case $1 in
    "$HOME"/*) printf '~%s\n' "${1#"$HOME"}" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

# Add $1 to PATH for future shells, through the profile the user's shell
# reads. The line checks PATH first, so nested shells do not repeat it.
add_to_path() {
  dir=$1
  # $HOME stays literal, for the profile's shell to expand.
  case $dir in
    "$HOME"/*) entry="\$HOME${dir#"$HOME"}" ;;
    *) entry=$dir ;;
  esac
  case ${SHELL:-} in
    */fish)
      profile=$HOME/.config/fish/conf.d/atelier.fish
      line="fish_add_path --path \"$entry\" $marker"
      ;;
    */zsh)
      profile=${ZDOTDIR:-$HOME}/.zshrc
      ;;
    */bash)
      profile=$HOME/.bashrc
      # macOS terminals start login shells, which read the first of these that
      # exists; creating ~/.bash_profile would hide an existing ~/.profile.
      if [ "$(uname -s)" = Darwin ]; then
        profile=$HOME/.bash_profile
        for login in .bash_profile .bash_login .profile; do
          if [ -f "$HOME/$login" ]; then
            profile=$HOME/$login
            break
          fi
        done
      fi
      ;;
    *)
      profile=$HOME/.profile
      ;;
  esac
  case ${SHELL:-} in
    */fish) ;;
    *) line="case \":\$PATH:\" in *\":$entry:\"*) ;; *) export PATH=\"$entry:\$PATH\" ;; esac $marker" ;;
  esac
  if ! grep -qsF "$marker" "$profile"; then
    mkdir -p "$(dirname "$profile")"
    if [ -s "$profile" ]; then printf '\n' >> "$profile"; fi
    printf '%s\n' "$line" >> "$profile"
  fi
  say ""
  say "Added $(tilde "$dir") to PATH in $(tilde "$profile")."
  say "Open a new terminal to use the atelier command; your agents can use it already."
}

target() {
  case $(uname -s) in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-musl ;;
    *) fail "there is no prebuilt atelier for $(uname -s); see https://github.com/$repo#build-from-source" ;;
  esac
  arch=$(uname -m)
  # A shell running under Rosetta reports x86_64 on Apple silicon.
  if [ "$os" = apple-darwin ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
    arch=arm64
  fi
  case $arch in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) fail "there is no prebuilt atelier for $arch; see https://github.com/$repo#build-from-source" ;;
  esac
  printf '%s-%s\n' "$arch" "$os"
}

main() {
  [ -n "${HOME:-}" ] || fail "HOME is not set"
  asset="atelier-$(target).tar.gz"
  version=${ATELIER_VERSION:-latest}
  if [ -n "${ATELIER_DOWNLOAD_URL:-}" ]; then
    base=${ATELIER_DOWNLOAD_URL%/}
  elif [ "$version" = latest ]; then
    base=https://github.com/$repo/releases/latest/download
  else
    base=https://github.com/$repo/releases/download/$version
  fi
  dir=${ATELIER_INSTALL_DIR:-$HOME/.local/bin}

  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  trap 'exit 1' HUP INT TERM

  say "Downloading $asset ($version)"
  download "$base/$asset" "$tmp/$asset"
  download "$base/SHA256SUMS" "$tmp/SHA256SUMS"
  expected=$(awk -v file="$asset" '$2 == file || $2 == "*" file { print $1 }' "$tmp/SHA256SUMS")
  [ -n "$expected" ] || fail "SHA256SUMS does not list $asset"
  [ "$(sha256 "$tmp/$asset")" = "$expected" ] || fail "$asset does not match its checksum"
  tar -xzf "$tmp/$asset" -C "$tmp" atelier

  mkdir -p "$dir"
  # Rename into place, so a running atelier keeps its file and the new one
  # appears whole.
  cp "$tmp/atelier" "$dir/.atelier.new"
  chmod 755 "$dir/.atelier.new"
  mv -f "$dir/.atelier.new" "$dir/atelier"
  shown=$(tilde "$dir")
  say "Installed $("$dir/atelier" --version) in $shown"

  if [ "${ATELIER_NO_CONNECT:-}" != 1 ]; then
    "$dir/atelier" connect ||
      fail "atelier is installed, but connecting it failed; fix the error above and run: $dir/atelier connect"
  fi

  case ":$PATH:" in
    *":$dir:"*)
      found=$(command -v atelier || true)
      if [ -n "$found" ] && [ "$found" != "$dir/atelier" ]; then
        say ""
        say "Note: another atelier at $found comes first on your PATH. Remove it"
        say "(for a cargo install: cargo uninstall atelier) so that $shown/atelier runs."
      fi
      ;;
    *)
      if [ "${ATELIER_NO_MODIFY_PATH:-}" = 1 ]; then
        say ""
        say "$shown is not on your PATH; your agents can start atelier without it."
        say "To run the atelier command yourself, add $shown to PATH."
      else
        add_to_path "$dir"
      fi
      ;;
  esac
}

# Everything runs from here, so a download cut short runs nothing.
main "$@"
