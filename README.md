<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/marmikshah/atelier-site/master/src/assets/logo-wordmark-dark.png">
    <img src="https://raw.githubusercontent.com/marmikshah/atelier-site/master/src/assets/logo-wordmark.png" width="384" alt="atelier">
  </picture>
</p>

<p align="center">
  Offline, headless pixel-art editing through a CLI and Model Context Protocol server.
</p>

<p align="center">
  <a href="https://github.com/marmikshah/atelier/actions/workflows/ci.yml"><img src="https://github.com/marmikshah/atelier/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/marmikshah/atelier/releases/latest"><img src="https://img.shields.io/github/v/release/marmikshah/atelier" alt="latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license"></a>
</p>

<p align="center">
  <img src="https://raw.githubusercontent.com/marmikshah/atelier-site/master/src/assets/studio-scene.gif" width="640" alt="Example animated pixel-art scene created with Atelier">
</p>

Atelier stores layered, animated documents locally and gives shell automation
and MCP clients the same 25-tool editing surface. Drawing and region operations,
palette constraints, visual analysis, checkpoints, replayable journals, and
spritesheet, GIF, and APNG export. No account, API key, outbound service, or
graphical environment.

It is also an experiment — [see below](#a-personal-note).

---

## Install

```sh
curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh
```

Installs the binary after verifying its published SHA-256. Ubuntu 22.04+ on
x86_64 or aarch64. Elsewhere, run the [container](docs/mcp.md#docker) — it is
the practical route on macOS, Windows, and other Linux distributions.

<details>
<summary>Other ways to install</summary>

- **Ubuntu archive** — binary, README, license, and checksum for x86_64 and
  aarch64 on the [latest release](https://github.com/marmikshah/atelier/releases/latest)
- **From source** — `cargo install --locked --path crates/atelier`, or
  `tools/install.sh --source` to build this checkout and install it

Native installation on Windows and on non-Ubuntu Linux is unsupported. macOS
builds and passes the test suite from source, but ships no release binary and
has no daemon.
</details>

## The first thirty seconds

```sh
atelier call doc_new '{"name":"cat","width":32,"height":32}'
# {"doc_id":"550e8400-e29b-41d4-a716-446655440000", ...} — use the returned id
atelier call doc_draw '{"doc_id":"550e8400-…","layer":0,"frame":0,"op":"fill_cel","color":[224,160,80]}'
atelier call doc_look '{"doc_id":"550e8400-…","out_path":"/tmp/cat.png"}'
```

That's the whole setup. No server, no daemon, no registration.

<p align="center">
  <code>doc_new</code> → <code>paint</code> → <b><code>doc_look</code></b> → <i>fix</i> → <code>doc_export</code>
</p>

## What it does

| Area | Included functionality |
|---|---|
| Document model | Layers, frames, animation tags, locked palettes, explicit checkpoints, persisted revisions |
| Editing | Pixel primitives, region operations, effects, grids, palette generation and snapping |
| Inspection | Rendered previews, region dumps, silhouettes, component analysis, frame diffs, seam reports, animation audits, critique |
| Output | Spritesheets with JSON metadata, GIF, APNG, PNG previews |
| Reproducibility | Versioned per-document JSONL journals, atomic deterministic replay |
| Recovery | Bounded checkpoints, deterministic portable document archives |
| Deployment | Ubuntu binary, static Alpine container, CLI, stdio MCP, authenticated HTTP MCP |

## How it works

**Nothing is implicit.** `doc_new` returns a UUID, and every later call carries
it. Layer and frame targets are explicit too. There is no active document, no
inferred name, no transport default.

**One dispatch path.** CLI, replay, stdio, and HTTP execute the same payload
and log the same line. Anything you can do in a shell, an agent can do over MCP.

**Art rebuilds itself.** Every document journals the deterministic calls that
made it, so `atelier replay <id>` reproduces the same pixels anywhere. Nothing
to enable.

**Agents can see.** `doc_look` returns rendered pixels, and the analysis tools
return structured critique — so the loop is draw, look, fix, rather than draw
and hope.

## Run it as an MCP server

For clients that only speak MCP, the same binary is the server:

```sh
atelier install          # shared background daemon on 127.0.0.1:8765/mcp
```

Or configure a stdio server whose command is simply `atelier`. Authentication,
Docker, remote access, and troubleshooting are in **[docs/mcp.md](docs/mcp.md)**.

## Documentation

- **[docs/cli.md](docs/cli.md)** — the complete CLI, journals, stores, backups, skills
- **[docs/mcp.md](docs/mcp.md)** — MCP daemon, auth, Docker, troubleshooting
- **[tool reference](https://marmikshah.github.io/atelier/tools.html)** — all 25 tools
- **[CHANGELOG.md](CHANGELOG.md)** — what changed, and when

## A personal note

Atelier began as one question: can agents, using only tool calls, make art
that's genuinely good enough to ship in a game?

**100% of this code was written by AI.** Not assisted — written. Opus did the
heavy lifting early on, with GPT 5.6 Sol and Fable 5 carrying the newer work,
and the code has been through several rounds of AI review and revision. I have
not written a single line. My part was direction: holding the project to the
same standards I use where I *do* still write the code.

It's an ongoing experiment, and I'll keep running it against other model
families to see how each one holds a brush.

If atelier helps you — as a tool, a reference, or a kick-start on your own game
— that makes me genuinely happy. The tokens are already spent; the least they
can do is be useful to you too.

> [!WARNING]
> **No human has reviewed this code, line by line — not any of it.** Tests,
> static analysis, locked dependencies, and release gates all pass, and none of
> that is the same as a person having read it. Assume bugs and security issues
> I haven't caught, and breaking changes in any release. Review the code and
> isolate important data before using Atelier in a production workflow.

> [!NOTE]
> **Versions restarted at `0.1.0`, and only the newest release is ever
> available.** The project used to number releases `1.0.0` through `1.9.1`,
> which claimed a stability it never had. `0.y` says what is actually true:
> this may break.
>
> Publishing a new release removes the one before it, along with its tag and
> its container images — maintaining several broken versions at once is not
> something I want to take on right now. Superseded releases are not archived
> and cannot be recovered, so when an upgrade needs migration steps, those
> steps ship with the release that requires them.

## Contributing

Thank you for wanting to — it means something that you got this far into the
project. **Atelier is closed to outside pull requests**, though, for the reason
above: your change would land in a codebase nobody has read closely enough to
review it against, me included. [CONTRIBUTING.md](.github/CONTRIBUTING.md) has
the longer version.

Bug reports and questions are very welcome as issues. Vulnerabilities go
through [SECURITY.md](.github/SECURITY.md), privately. And it's MIT licensed —
fork it and take it wherever you like, no permission needed.

## License

[MIT](LICENSE) © Marmik Shah
