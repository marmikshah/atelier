# Repository guidance

Keep this file small. Human-facing project policy belongs in `README.md`,
`.github/CONTRIBUTING.md`, and `docs/`; this file only records durable
instructions for coding agents.

## What ships

atelier is an offline, headless pixel-art editor exposed through a CLI and MCP.
The shipped dependency tower is `atelier-core` → `atelier-studio` →
`atelier-mcp` → `atelier`.

## Commands

- `make check` — complete non-mutating gate: metadata, formatting, Clippy,
  rustdoc, tests, example replays, and tool-reference rendering.
- `make fmt` — apply Rust formatting.
- `make docs` — generate a local `site/tools.html` preview. Pages generates it
  during deployment.
- `make release` — build `target/release/atelier`.

The committed `Cargo.lock` is intentional because this workspace ships release
binaries. Keep builds locked and include lockfile changes with dependency
updates.

## Working agreements

- Keep the functional core independent of MCP and async.
- Route CLI, MCP, and replay calls through the same dispatch path.
- Keep every advertised tool reachable and update the registry tests in the
  same change. `make check` verifies that tool documentation still renders.
- Prefer `Result` to panics and keep Clippy/rustdoc warning-free.
- Avoid new production dependencies unless they materially simplify the code.
- Preserve unrelated user changes in a dirty worktree.

## Releases

Only the maintainer creates or pushes version tags. Agents may prepare a version
PR but must not publish a release. Never bump or tag `2.0.0`; that is the
maintainer's manual-review milestone. Follow `docs/RELEASING.md`.
