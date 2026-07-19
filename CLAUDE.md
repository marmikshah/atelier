# CLAUDE.md — atelier

Agent onboarding. Keep it short and current; `make` is the entry point.

## What this is

atelier is a headless pixel-art editor driven from the CLI or over MCP — layered/animated
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
| `make test` | test suite + replays every `docs/examples` recipe |
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
| `atelier call <tool> '<json>'` | one tool call, in-process — the front door (`--file`/`--stdin`/`--home`) |
| `atelier tools [--html\|--schema <name>]` | list the tool surface / emit the reference page / dump one input schema |
| `atelier replay <recipe.json or doc-id>` | replay a recipe file, or rebuild a document from its own journal |
| `atelier library [rm …]` | inspect or prune the document store (destructive: confirms first) |
| `target/release/atelier` | MCP add-on: stdio server (a client spawns it) |
| `target/release/atelier --http 127.0.0.1:8765` | MCP add-on: HTTP server |
| `atelier install` / `status` / `uninstall` | background daemon (launchd / systemd) |
| `git config core.hooksPath .githooks` | wire the format/lint/test git hooks (once) |

## Architecture

A Cargo workspace, strict dependency tower (see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)):

- `atelier-core` — document model + raster ops (no async, no MCP).
- `atelier-studio` — the `Studio` facade (the library API): one method per tool; single draw/fx ops route through `doc_draw`/`doc_fx` over the core op registry.
- `atelier-mcp` — `Atelier::dispatch`, the one path every caller (CLI, MCP
  stdio/HTTP, replay, agent) shares, plus the rmcp `#[tool]` server; advertises
  all **28** tools, with no profile filter. The count is pinned by a test,
  another test fails if a tool description names a tool that no longer exists,
  and a third fails if an advertised tool has no dispatch arm — change the
  surface, update the docs in the same commit. A tool earns its place by being
  called: everything with no caller in either an agent transcript or a shipped
  recipe was deleted, not hidden.
- `atelier` — the binary: arg parsing, `atelier call` (the CLI front door), the
  daemon installer, the `replay` runner, and the gated `agent` mode.

**The one online exception.** Everything above is offline, keyless and
deterministic. `atelier agent` is the single command that reaches the network:
it drives an OpenAI-style API to draw a task on its own. It is behind the
`agent` cargo feature (OFF by default, so the shipped binary links no HTTP
stack), reads `OPENAI_API_KEY` from the env, and is never part of a default
install. It dispatches the model's tool calls in-process through
`Atelier::dispatch`, so it reuses the whole validated tool path rather than a
second copy.

The workflow guidance is a typed registry (`crates/atelier/src/skills.rs`): Rust
owns each skill's metadata and the renderers (the standard `SKILL.md`, the agent
system prompt); the prose stays markdown in `crates/atelier/skills/*.md`.
`atelier skills install --for claude|kimi|cursor|all` writes the `SKILL.md`
files into that agent's skills dir (default `~/.claude/skills/`); `atelier agent`
renders its prompt from the same registry. A test fails if a skill names a tool
that no longer exists.
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
  `docs/examples/*.json` double as integration tests — `make test` replays
  each one through the real binary.

## Dev notes

- Config is via `ATELIER_*` env vars (see `.env.example`); the tool holds no secrets.
- Wire the format/lint/test gate into git once: `git config core.hooksPath .githooks`.
