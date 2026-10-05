# Development

[README](../README.md) · [Getting started](getting-started.md) · [Running Atelier](running.md)

Run commands below from the repository root. See [Contributing](../.github/CONTRIBUTING.md)
for the project's contribution policy.

## Repository map

| Path | Responsibility |
| --- | --- |
| `crates/atelier/` | CLI commands, replay, archives, agent skills, and Linux daemon |
| `crates/atelier-mcp/` | Tool schemas and dispatch, plus stdio and HTTP transports |
| `crates/atelier-studio/` | Storage, transactions, journals, checkpoints, and analysis |
| `crates/atelier-core/` | Document model, pixel operations, rendering, and exports |
| `site/` | React + TypeScript showcase website, built with Vite |
| `showcase/` | Frozen briefs, recorded runs, replay journals, and GIFs |
| `tools/` | Development checks, showcase runner, and container smoke test |

An edit travels through these layers:

```text
CLI call or MCP request
  → atelier-mcp: validate arguments and dispatch the tool
  → atelier-studio: load, edit, journal, and persist the document
  → atelier-core: operate on pixels, layers, and frames
```

Start with [main.rs](../crates/atelier/src/main.rs) for CLI routing,
[server/mod.rs](../crates/atelier-mcp/src/server/mod.rs) for tool dispatch, and
[params.rs](../crates/atelier-mcp/src/server/params.rs) for input schemas.
The core has no MCP or server dependencies. The website displays recorded
showcase results; it is separate from the editor.

## Build and check the editor

Use Linux or macOS with a native build toolchain and Rust installed through
rustup. [rust-toolchain.toml](../rust-toolchain.toml) pins the development
version and components; the minimum supported Rust version is 1.88.
Python 3 is needed for showcase tooling.

```sh
cargo build --locked -p atelier
./target/debug/atelier --help
tools/check.sh
python3 -m unittest discover -s tools/tests
python3 tools/showcase.py verify
```

`tools/check.sh` checks formatting, strict Clippy, rustdoc, and Rust tests.
The Python tests cover showcase tooling. The replay verifier rebuilds every
committed showcase GIF offline and checks for byte-identical output; build the
editor first so `target/debug/atelier` exists.

For an optimized binary or browsable Rust API documentation:

```sh
cargo build --release --locked -p atelier
cargo doc --locked --no-deps --open
```

## Work on the website

Use Node.js 24+:

```sh
npm ci --prefix site
npm --prefix site run dev
```

The dev command prepares showcase data and starts Vite with hot reload. Before
submitting site changes, run:

```sh
npm --prefix site run check
npm --prefix site run build
```

The check covers formatting, strict TypeScript, and comparison tests. The
production build is written to `site/dist`.

## Understand and reproduce the showcase

[showcase/runs.json](../showcase/runs.json) describes the runs and tasks.
Each recorded animation has a JSONL recipe under `showcase/replays/` and a GIF
under `showcase/gifs/`. The website prepares its data from these committed files.

To inspect one recipe in an isolated store:

```sh
./target/debug/atelier replay showcase/replays/opus-4.8/cat.jsonl --home ./replay-store
./target/debug/atelier library --home ./replay-store
```

Use the returned document ID with the tools from [Getting started](getting-started.md),
adding `--home ./replay-store` to each call. To collect new runs, start with
`python3 tools/showcase.py run --help`. Collection requires an authenticated
Codex CLI; verifying existing artwork does not.

## CI and publishing

CI checks native builds on Ubuntu and macOS, the minimum supported Rust version,
and showcase tooling. A separate workflow verifies replay bytes. Linux container
checks run on pushes to `master` and non-draft PRs targeting `master`.

The Pages workflow checks and builds website changes, then publishes the showcase
from `master`. No binary releases or container images are published. See
[Running Atelier](running.md#docker-compose) for local container use.
