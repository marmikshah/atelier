<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-wordmark-dark.png">
    <img src="assets/logo-wordmark.png" width="384" alt="atelier">
  </picture>
</p>

Atelier is an offline, headless pixel-art editor for CLI and MCP clients.
Its 26 tools edit layered animations, inspect pixels, and export PNGs,
spritesheets, GIFs, APNGs, and TrueType pixel fonts. Documents stay local.

[Explore the showcase](https://marmikshah.github.io/atelier/): compare original
animations, filter models and briefs, download replays, or export run data.

## Architecture

```text
crates/
├── atelier-core/    Document format, raster operations, rendering and exports
├── atelier-studio/  Storage, transactions, journals, checkpoints and analysis
├── atelier-mcp/     Shared tool dispatch, schemas, stdio and HTTP transports
└── atelier/         CLI, replay, archives, agent skills and Linux daemon
site/                React + TypeScript showcase, built with Vite
showcase/            Frozen briefs, recorded runs, replay journals and GIFs
tools/               Development checks, showcase runner and container smoke
```

## Run

Build from a checkout with Rust 1.88+; the pinned toolchain is used for development.
Linux and macOS support native builds. Windows uses Docker; the daemon needs Linux.

```sh
cargo install --locked --path crates/atelier
atelier init                         # opt into a directory-local .atelier store
atelier call doc_new '{"name":"cat","width":32,"height":32}'
atelier call doc_look '{"doc_id":"<returned-id>","out_path":"preview.png"}'
atelier                              # stdio MCP server, launched by your client
atelier --http                       # foreground HTTP at 127.0.0.1:8765/mcp
atelier install --port 8765           # optional systemd --user daemon
atelier skills install --for codex    # also claude, kimi, cursor, or all
atelier --help                       # commands, store policy and environment
atelier tools --markdown             # current tool reference; --schema NAME for JSON
```

Documents use explicit UUIDs; tools safely reset unsupported or corrupt saved data.
`atelier library verify` inspects without changes; archives preserve UUIDs.

## Develop and release

```sh
tools/check.sh                       # formatting, strict Clippy, rustdoc and tests
cargo build --locked -p atelier
python3 -m unittest discover -s tools/tests
python3 tools/showcase.py verify      # reproduce all committed GIFs, offline
python3 tools/showcase.py run --help  # collect new runs with authenticated Codex
npm ci --prefix site                 # Node.js 24+
npm --prefix site run dev             # website with hot reload
npm --prefix site run check           # formatting, strict TypeScript and tests
npm --prefix site run build           # production website → site/dist
cargo build --release --locked -p atelier
```

No releases or images are published. With `ATELIER_HTTP_TOKEN` set, run
`docker compose up -d --build`; it binds localhost and persists a named volume.
Check an image with `tools/container-smoke.sh IMAGE`; detailed instructions use `--help`.
CI checks native builds, MSRV, containers and replay bytes; master publishes the website.

See [Contributing](.github/CONTRIBUTING.md) and [Security](.github/SECURITY.md). [MIT](LICENSE) © Marmik Shah
