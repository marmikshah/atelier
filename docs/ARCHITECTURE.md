# Architecture

atelier is a Cargo **workspace** of four crates arranged as a strict dependency
tower: a pure document/raster core, the studio operations that act on it, the MCP
server that exposes those operations as tools, and the binary that wires up
transports. Each layer depends only on the ones below it — there are no cycles.

```
            ┌─────────────────────────────────────────────┐
  binary →  │  atelier            main · service · replay  │  arg parsing, daemon,
            │                     (the `atelier` binary)   │  replay client
            └───────────────────────┬─────────────────────┘
                                    │ depends on
            ┌───────────────────────▼─────────────────────┐
  shell  →  │  atelier-mcp        server · recipe          │  rmcp #[tool] router,
            │                                              │  stdio + HTTP, web views
            └───────────────────────┬─────────────────────┘
                                    │
            ┌───────────────────────▼─────────────────────┐
  core   →  │  atelier-studio     studio · craft ·         │  the Studio facade:
            │                     analysis · reference     │  one fn per operation
            └───────────────────────┬─────────────────────┘
                                    │
            ┌───────────────────────▼─────────────────────┐
  model  →  │  atelier-core       document · raster        │  layered/animated doc
            │                     (no async, no MCP)       │  model + pixel ops
            └─────────────────────────────────────────────┘
```

This is a *functional core, imperative shell*: `atelier-core` and
`atelier-studio` are deterministic, synchronous, and dependency-light; all of the
async, networking, and protocol surface lives in `atelier-mcp` and the binary.

## The crates

### `atelier-core` — the document model (~7.5k LOC, no async/MCP)

The functional core. Has no knowledge of MCP, tokio, or the network.

- **`document.rs`** — `Document`: an ordered stack of layers over a timeline of
  frames. A *cel* is the RGBA image for one (layer, frame). Holds the palette,
  tags, per-frame durations/pivots/boxes, blend modes, and the batch-op
  validator. Persists as a directory: `doc.json` (structure + cel file refs) plus
  one PNG per cel under `cels/L<layer>_F<frame>.png`.
- **`raster.rs`** — the pixel-level primitives the layers above compose: point/
  brush/line plotting and the anti-aliased `stroke_ribbon`; the 14 blend modes
  and `composite`; colour-space conversions (sRGB ⇄ OKLab/OKLCh/HSL) and ΔE; the
  pixel font glyphs; easing curves, fBm/perlin noise, 2-bone IK, and nearest-
  neighbour rotate/affine.

Dependencies: `image`, `serde`, `serde_json`, `png` (APNG encode).

### `atelier-studio` — the operations (~8k LOC)

The `Studio` facade: a flat document store rooted at `ATELIER_HOME` (default
`~/.atelier`), exposing **one public method per editor operation** — each takes
plain arguments and returns `Result<serde_json::Value, String>`. This is the
entire library API; the MCP layer is a thin wrapper over it.

- **`lib.rs`** (the `Studio` struct) — store/document lifecycle, layers/frames/
  tags, render and export (`doc_look`, sheet/GIF/APNG/atlas/tileset), and the
  small drawing entrypoints. `Studio::new()` reads `ATELIER_HOME`;
  `Studio::with_docs_dir(path)` roots a studio at an explicit directory (for
  embedding or tests, without touching process-global env).
- **`craft.rs`** — drawing, procedural and constructive ops: shapes, fills,
  gradients, noise/scatter, shadow/glow/bevel, the `doc_fx op=form` volume shader, and
  the `doc_figure`/`doc_walk` skeletal builders.
- **`analysis.rs`** — the "eye": critique, silhouette, palette, contrast,
  frame-diff, loop-seam and per-pixel diff-map reports that turn "does it look
  right?" into numbers.
- **`reference.rs`** — reference-image workflow: set/analyse a reference and score
  silhouette IoU + per-cell ΔE against it (`doc_ref_compare`).

Dependencies: `atelier-core`, `image`, `serde_json`, `dirs`.

### `atelier-mcp` — the MCP server (~4.8k LOC)

The imperative shell. Wraps `Studio` in an `Arc<Mutex<…>>` and exposes it.

- **`server.rs`** — the rmcp `#[tool]` router: **70 tools** (mutations grouped
  into op-dispatch tools like `doc_draw` / `doc_fx` / `doc_export` / `doc_batch`), one or one-family per studio
  operation, plus MCP resources (browse documents + renders) and packaged
  prompts. Runs over two transports that share the router — stdio (`run`) and
  streamable HTTP (`run_http`) — and, on HTTP, serves the live `/gallery`,
  `/playground` and `/live` web views with a Server-Sent-Events stream that
  pushes every mutating tool call. Also houses the `Recorder` that turns a live
  session into a replayable recipe.
- **`recipe.rs`** — the `Recipe`/`Step` format: the on-disk contract shared by the
  `Recorder` (writer) and the `atelier replay` runner (reader). Lives here, in the
  library crate, so anything embedding atelier can read/write recipes without the
  binary.

Dependencies: `atelier-core`, `atelier-studio`, `rmcp`, `axum`, `tokio`,
`tokio-stream`, `schemars`, `serde`.

### `atelier` — the binary (~0.8k LOC)

Thin wiring; produces the `atelier` executable.

- **`main.rs`** — arg/env parsing and transport selection (stdio vs `--http`,
  `--record`, subcommand dispatch).
- **`service.rs`** — installs/uninstalls the background daemon (launchd on macOS,
  `systemd --user` on Linux).
- **`replay.rs`** — the `atelier replay` runner: an MCP *client* that spawns this
  same binary as a child server over stdio and issues one `tools/call` per recipe
  step, strictly sequenced.

Dependencies: `atelier-mcp`, `tokio`, `serde`, `serde_json`.

## Request flow

A single drawing tool call travels straight down the tower and back:

```
agent → MCP tools/call → server.rs #[tool] handler
      → Studio::doc_<op>(args)              (atelier-studio)
      → Document / raster mutation          (atelier-core)
      → persist to ~/.atelier/documents/<id>/
      → render PNG → returned to the agent to look at
```

Because every mutation is one tool call and documents are an ordered sequence of
them, a piece of art is a **recipe** (`atelier-mcp::recipe`) that replays
deterministically — which is also how the `docs/examples/` integration tests work.

## Conventions

- **Tests live next to the code** they cover, as inline `#[cfg(test)]` modules
  (the bulk in `atelier-core` and `atelier-studio`). `cargo test` at the workspace
  root runs every crate; so do `cargo clippy --all-targets -- -D warnings` and
  `cargo fmt --all`.
- **Dependency versions are centralised** in the root `[workspace.dependencies]`;
  crates inherit with `<dep>.workspace = true`. Package metadata (version, license,
  repository) is shared via `[workspace.package]`.
- **The release binary is unchanged at `target/release/atelier`** — the virtual
  workspace doesn't move it, so `install.sh`, the `Makefile`, and the release
  workflow need no path changes.
