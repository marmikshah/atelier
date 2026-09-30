<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-wordmark-dark.png">
    <img src="assets/logo-wordmark.png" width="384" alt="atelier">
  </picture>
</p>

Atelier is an offline, headless pixel-art editor for shell automation and MCP
clients. Its 25 tools edit layered animations, inspect rendered pixels, and
export PNGs, spritesheets, GIFs, and APNGs. Documents and replay journals stay
local; the editor needs no account or outbound service.

**[Explore the model showcase →](https://marmikshah.github.io/atelier/)**
Claude, Codex, and Kimi draw the same ten briefs. Every animation includes its
original replay and recorded run statistics.

## Build and draw

Build from source; there are no published releases or container images:

```sh
git clone https://github.com/marmikshah/atelier.git
cd atelier
cargo install --locked --path crates/atelier
atelier call doc_new '{"name":"cat","width":32,"height":32}'
```

Use the returned `doc_id` in later calls, with explicit layer and frame targets:

```sh
atelier call doc_draw '{"doc_id":"<returned-id>","layer":0,"frame":0,"op":"fill_cel","color":[224,160,80]}'
atelier call doc_look '{"doc_id":"<returned-id>","out_path":"/tmp/cat.png"}'
```

Linux supports the editor and the `systemd --user` daemon. macOS builds and
passes tests but has no daemon. Use Docker on Windows. Rust 1.88 is the minimum
supported compiler; `rust-toolchain.toml` selects the development toolchain.

## Commands and documents

The binary owns its reference: `atelier --help`, `atelier tools --markdown`,
and `atelier tools --schema <tool>` describe the commands and tool arguments.

```sh
atelier init                         # create a directory-local .atelier store
atelier library                      # list documents
atelier library verify               # check stored documents without changes
atelier library pack <id> --out art.atelierpack
atelier library unpack art.atelierpack
atelier replay <journal|id> --home /tmp/demo
atelier skills install --for codex   # also claude, kimi, cursor, or all
atelier skills show sprite           # also scene or review
```

Store selection is `--home DIR`, then `ATELIER_HOME`, then a local `./.atelier`,
then `~/.atelier`. Each document records deterministic editing calls in
`documents/<id>/recipe.jsonl`; replay stages the entire document before
publishing it. Archives preserve UUIDs and refuse collisions unless replacement
is explicitly requested. Checkpoints provide bounded local recovery.

CLI, replay, stdio, and HTTP use one dispatch path. There is no active document
or inferred target. Mutations may include `expected_revision` to reject a stale
write with `revision_conflict`; omitting it keeps last-write-wins behavior.

## MCP and Docker

Configure a stdio MCP client to launch `atelier`, or install the Linux daemon:

```sh
atelier install --port 8765
atelier status
# Endpoint: http://127.0.0.1:8765/mcp
```

`atelier --http [ADDR]` runs a foreground HTTP server. Non-loopback listeners
require `ATELIER_HTTP_TOKEN`; when set, every request needs the matching bearer
token. Use a TLS reverse proxy for remote access. HTTP file access requires
`ATELIER_IMPORT_ROOT` or `ATELIER_EXPORT_ROOT` and relative paths under those
directories. CLI and stdio retain normal local filesystem access.

Set `ATELIER_HTTP_TOKEN` in your environment, then run the container:

```sh
docker compose up -d --build
```

It exposes `127.0.0.1:8765/mcp` and persists documents in a named volume.
`ATELIER_PLATFORM=linux/arm64` selects ARM64; CI checks `linux/amd64`.
Daemon logs: `journalctl --user -u atelier -f`. Remove it with `atelier uninstall`.

## Develop and build the showcase

```sh
tools/check.sh                       # format, lint, rustdoc, tests, site build
cargo fmt --all                      # apply formatting
cargo build --release --locked -p atelier
cargo build --locked -p atelier
tools/showcase-check.sh               # reproduce every committed GIF
python3 tools/build-site.py           # static site → target/site, no dependencies
python3 -m http.server 8000 --directory target/site
```

The website uses plain HTML, CSS, and JavaScript. Its build reads
`showcase/runs.json`, frozen briefs, GIFs, and replay journals; the generated
site stays in ignored `target/site`. GitHub Pages publishes it when changes
reach `master`. CI is self-contained and uses only public actions.

To collect another showcase run with an authenticated Codex CLI:

```sh
python3 tools/run-showcase.py --model gpt-6.1-sol --effort max
```

The runner uses isolated stores and the shipped sprite skill, verifies replay
bytes, and writes transcripts, art, and usage to `target/showcase/`. Its
`--help` describes task selection and resuming interrupted sessions. Preserve
the counting-method caveats in `showcase/runs.json` when adding results.

## About the project

Atelier asks whether agents working through tools can make art useful in games.
All code was written by AI and has had no line-by-line human review. Assume
bugs and breaking changes; review the code and isolate important data before
using it in production.

Outside pull requests are closed; bug reports and questions are welcome.
Report vulnerabilities privately through [SECURITY.md](.github/SECURITY.md).
See [CONTRIBUTING.md](.github/CONTRIBUTING.md) for participation details.

[MIT](LICENSE) © Marmik Shah
