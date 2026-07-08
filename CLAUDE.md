# CLAUDE.md — atelier

Agent onboarding. Keep it short and current; `make` is the entry point.

## What this is

atelier is a headless pixel-art editor exposed as an MCP server — layered/animated
documents, drawing primitives, a see-and-critique loop, and engine-ready
PNG/sheet/GIF/atlas export. One static Rust binary; no API keys, no network,
fully deterministic. See [README.md](README.md) and [docs/](docs/).

## Entry point

**Everything is a `make` target — never run ad-hoc scripts.** `make help` lists them.

| target | use |
|--------|-----|
| `make run` | release build, then run the HTTP MCP server |
| `make stdio` | run the stdio MCP server |
| `make test` | test suite |
| `make pre-commit-checks` | format-check + clippy gate (what the hooks run) |
| `make branding` | brand art (the gallery is recipe-made) |
| `make hooks` | install the `.githooks` (pre-commit + pre-push) |
| `make release` | optimized binary at `target/release/atelier` |
| `make clean` | wipe build artifacts |

## Architecture

A Cargo workspace, strict dependency tower (see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)):

- `atelier-core` — document model + raster ops (no async, no MCP).
- `atelier-studio` — the `Studio` facade: one method per editor operation (the library API).
- `atelier-mcp` — the rmcp `#[tool]` server (stdio + HTTP); advertises a ~28-tool
  **core** profile by default, the full 66 with `ATELIER_PROFILE=full`.
- `atelier` — the binary: arg parsing, the daemon installer, the `replay` runner.

## Hard constraints

- Functional core decoupled from the MCP shell; `Result` over `panic!`; clippy clean.
- Tests live next to the code as inline `#[cfg(test)]` modules; keep them green.
- Every document is an ordered sequence of tool calls, so art is a replayable
  **recipe** (`docs/examples/*.json`) — these double as integration tests.

## Dev notes

- Config is via `ATELIER_*` env vars (see `.env.example`); the tool holds no secrets.
- Run `make hooks` once to wire the format/lint/test gate into git.
