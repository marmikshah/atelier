# CLAUDE.md — atelier

Agent onboarding. Keep it short and current; `make` is the entry point.

## What this is

atelier is a headless pixel-art editor exposed as an MCP server — layered/animated
documents, drawing primitives, a see-and-critique loop, and engine-ready
PNG/sheet/GIF/atlas export. One static Rust binary; no API keys, no network,
fully deterministic. See [README.md](README.md) and [docs/](docs/).

## Entry point

The **Makefile builds/tests/lints only**. Running the server, installing the
daemon, generating art, and building the image are **direct commands** — the
binary's own subcommands are the interface (an installed user has no Makefile).

**Build/test (`make help` lists them):**

| target | use |
|--------|-----|
| `make release` | optimized binary at `target/release/atelier` |
| `make build` | debug build |
| `make test` | test suite |
| `make pre-commit-checks` | format-check + clippy gate (what the hooks run) |
| `make check` | fmt + clippy + tests |
| `make docs` | regenerate `site/tools.html` from the live tool registry |
| `make docs-check` | fail if `site/tools.html` is stale (CI runs this) |
| `make clean` | wipe build artifacts |

Tool descriptions are the model's only guide, and `site/tools.html` is generated
from them — change a description, run `make docs` in the same commit.

**Run / operate (direct):**

| command | use |
|---------|-----|
| `target/release/atelier` | stdio MCP server (a client spawns it) |
| `target/release/atelier --http 127.0.0.1:8765` | HTTP MCP server |
| `atelier install` / `status` / `uninstall` | background daemon (launchd / systemd) |
| `atelier tools [--html]` | list the tool surface / emit the reference page |
| `atelier library [rm …]` | inspect or prune the document store (destructive: confirms first) |
| `atelier replay <recipe.json or doc-id>` | replay a recipe file, or rebuild a document from its own journal |
| `git config core.hooksPath .githooks` | wire the format/lint/test git hooks (once) |

## Architecture

A Cargo workspace, strict dependency tower (see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)):

- `atelier-core` — document model + raster ops (no async, no MCP).
- `atelier-studio` — the `Studio` facade (the library API): one method per tool; single draw/fx ops route through `doc_draw`/`doc_fx` over the core op registry.
- `atelier-mcp` — the rmcp `#[tool]` server (stdio + HTTP); advertises all **28**
  tools, with no profile filter. The count is pinned by a test, and another test
  fails if a tool description names a tool that no longer exists — change the
  surface, update the docs in the same commit. A tool earns its place by being
  called: everything with no caller in either an agent transcript or a shipped
  recipe was deleted, not hidden.
- `atelier` — the binary: arg parsing, the daemon installer, the `replay` runner.

The workflow guidance lives in `.claude/skills/` (atelier-sprite / atelier-scene /
atelier-review), not in the server: `install.sh` copies them into the user's
`~/.claude/skills/`. A test fails if a skill names a tool that no longer exists.
The MCP server ships no prompts — the skills replaced them.

## Hard constraints

- **Never cut 2.0.0.** That version is the human-review milestone: no agent may
  bump the workspace to 2.0.0, tag v2.0.0, or publish a 2.0.0 release. It is
  tagged by the maintainer, by hand, after a full manual review. Stay on 1.x.
- Functional core decoupled from the MCP shell; `Result` over `panic!`; clippy clean.
- Tests live next to the code as inline `#[cfg(test)]` modules; keep them green.
- Every document is an ordered sequence of tool calls, so art is a replayable
  **recipe**. Documents journal themselves to `<store>/<id>/recipe.jsonl` (JSON
  Lines, on by default, mutations only — `is_journaled` in the MCP server is the
  read/write classifier, and its default is to record). The authored recipes in
  `docs/examples/*.json` double as integration tests.

## Dev notes

- Config is via `ATELIER_*` env vars (see `.env.example`); the tool holds no secrets.
- Wire the format/lint/test gate into git once: `git config core.hooksPath .githooks`.
