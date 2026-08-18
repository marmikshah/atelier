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

Atelier stores layered, animated documents locally and exposes the same 25-tool
editing surface to shell automation and MCP clients. It includes drawing and
region operations, palette constraints, visual analysis, checkpoints,
replayable journals, and spritesheet, GIF, and APNG export. It does not require
an account, API key, outbound service, or graphical environment.

---

## Install

The native release supports Ubuntu 22.04 or newer on x86_64. The supported
container release is an Alpine-based `linux/amd64` image.

```sh
curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh
```

Installs the binary after verifying its published SHA-256. That's the
whole setup — drive it from any shell:

```sh
atelier call doc_new '{"name":"cat","width":32,"height":32}'
# {"doc_id":"550e8400-e29b-41d4-a716-446655440000", ...} — use the returned id
atelier call doc_draw '{"doc_id":"550e8400-e29b-41d4-a716-446655440000","layer":0,"frame":0,"op":"fill_cel","color":[224,160,80]}'
atelier call doc_look '{"doc_id":"550e8400-e29b-41d4-a716-446655440000","out_path":"/tmp/cat.png"}'
```

Every tool is available through `atelier call`: stdout contains the JSON
result, while exit codes distinguish success, tool errors, and invalid calls.
`atelier tools` lists the surface and `atelier tools --schema <name>` prints an
individual input schema. Shell-based automation needs no server registration
or background process.

<p align="center">
  <code>doc_new</code> → <code>paint</code> → <b><code>doc_look</code></b> → <i>fix</i> → <code>doc_export</code>
</p>

### Other ways

- **Ubuntu archive** — binary, README, license, and checksum on the
  [latest release](https://github.com/marmikshah/atelier/releases/latest)
- **Source on Ubuntu x86_64** — `cargo install --locked --path crates/atelier`,
  or `tools/install.sh --source` to build this checkout and install it

Native installations on macOS, Windows, Linux distributions other than Ubuntu,
and non-x86_64 processors are not supported.

## Optional: run as an MCP server

For clients that only speak MCP, the same binary is an MCP server — one
command sets up a shared background daemon (`systemd --user`):

```sh
atelier install
# MCP HTTP port [8765]:
```

The prompt appears on both first install and reinstall; reinstall defaults to
the currently configured port. For scripts, use `atelier install --port 9123`.
The installed systemd service is intentionally loopback-only. To serve another
interface, run an authenticated foreground process or the supported container:

```sh
export ATELIER_HTTP_TOKEN="$(openssl rand -hex 32)"
ATELIER_HTTP=0.0.0.0:9123 atelier
```

Atelier deliberately does not rewrite third-party client configuration. Point
your MCP client at the endpoint printed by `atelier status`:

```text
http://127.0.0.1:8765/mcp
```

Or configure a stdio MCP server whose command is simply `atelier`. Stdio needs
no daemon: each client starts its own process, while all processes still resolve
the same global or directory-local document store.

Keep your client's normal approval prompts enabled: Atelier can write exports
and delete documents.

Non-loopback HTTP refuses to start without `ATELIER_HTTP_TOKEN`. Whenever that
variable is set, including on loopback, clients must send
`Authorization: Bearer <token>`. Use a TLS reverse proxy on untrusted networks;
the built-in listener is plain HTTP. `ATELIER_ALLOWED_HOSTS` adds Host-header
validation but does not replace authentication.

HTTP external file access is off by default. Set `ATELIER_IMPORT_ROOT` and/or
`ATELIER_EXPORT_ROOT` to existing directories to enable it, then pass only
relative `path`/`out_path` values beneath those roots. Absolute paths, parent
traversal, and symlink escapes are rejected. Direct CLI and stdio calls retain
normal local filesystem access. Request bodies are capped at 1 MiB with a
30-second upload deadline, and at most 64 requests run concurrently.

### Docker

The second supported release is a small Alpine `linux/amd64` image with a
static musl binary and no runtime packages. It serves the same HTTP MCP
endpoint:

```sh
export ATELIER_HTTP_TOKEN="$(openssl rand -hex 32)"
docker run -d --platform linux/amd64 \
  -p 127.0.0.1:9123:8765 \
  -v atelier-data:/data \
  -e ATELIER_HTTP_TOKEN \
  ghcr.io/marmikshah/atelier:latest
```

Documents persist in the `atelier-data` volume, so they survive restarts.
Here `9123` is the configurable host port; the Alpine container keeps its
internal endpoint on `8765`.
Configure the same bearer token in the MCP client's HTTP headers. The image
contains no default token and refuses to start its network listener without
one. Optional import/export directories must be mounted and enabled with the
corresponding root variables.
There's a [`docker-compose.yml`](docker-compose.yml) if you'd rather keep it
declarative.

### MCP notes

- **Your client shows 0 tools** — restart it after registering; MCP clients read
  their server list at session start.
- **stdio or daemon?** The daemon is one shared server and store, and it
  survives reboots; stdio means each client spawns its own `atelier` process.
  Both use the same resolved store. (The CLI needs neither transport.)
- **The selected port is already in use** — rerun `atelier install` and choose
  another port (`--port PORT` in scripts). `atelier status` prints the installed
  endpoint; `atelier uninstall` stops the daemon.
- **Where are the logs?** Use `journalctl --user -u atelier -f` for the Ubuntu
  daemon. Verbosity is controlled by `ATELIER_LOG` (`RUST_LOG` syntax). In
  stdio mode the same log goes to the spawning client's stderr.
- **Uninstall everything** — `install.sh uninstall` (or `atelier uninstall` for
  just the daemon). Your documents in `~/.atelier` are kept; delete that
  directory too if you want them gone.

## Capabilities

| Area | Included functionality |
|---|---|
| Document model | Layers, frames, animation tags, locked palettes, explicit checkpoints, and persisted revisions |
| Editing | Pixel primitives, region operations, effects, grids, palette generation, and palette snapping |
| Inspection | Rendered previews, region dumps, silhouettes, component analysis, frame diffs, seam reports, animation audits, and critique reports |
| Output | Spritesheets with JSON metadata, GIF, APNG, and PNG previews |
| Reproducibility | Versioned per-document JSONL journals and atomic deterministic replay |
| Recovery | Bounded checkpoints and deterministic portable document archives |
| Deployment | Ubuntu x86_64 binary, static Alpine amd64 container, CLI, stdio MCP, and authenticated HTTP MCP |

Image-producing inspection calls return pixels only when requested. Editing
calls return compact structured results, which keeps automated feedback loops
predictable without embedding a preview after every mutation. Inline MCP PNGs
are limited to 8 MiB before base64 encoding; larger renders already written to
`out_path` return their report without a duplicate inline image, while other
oversized renders fail with reduction/output guidance.

## The CLI

```sh
atelier call <tool> '<json>'   # one tool call, in-process — the front door
atelier call <tool> --file ops.json   # args from a file (or --stdin)
atelier call doc_draw '{"doc_id":"550e8400-e29b-41d4-a716-446655440000","layer":1,"frame":0,"op":"clear_cel"}'
atelier tools [--schema <name]]       # the tool surface / one input schema
atelier replay <recipe|id>     # rebuild a document from its journal
atelier library                # what's in your document store
atelier library verify [--json] # validate metadata, cels, references, journals
atelier library pack <id> --out art.atelierpack # write a portable backup
atelier library unpack art.atelierpack          # restore its original UUID
atelier skills install         # write the skills (--for claude|codex|kimi|cursor|all)
```

And the MCP add-on:

```sh
atelier                    # stdio MCP server (your client spawns it)
atelier --http             # HTTP server at 127.0.0.1:8765/mcp
atelier install            # background daemon; asks for port (default 8765)
atelier install --port 9123 # non-interactive install/reinstall
atelier status             # daemon state, endpoint, and log locations
atelier uninstall          # stop and remove the daemon
```

Every call — CLI, replay, stdio or HTTP — travels one dispatch path, so it logs
the same line to stderr: tool name, op, target document, caller, duration, and
the error text when a call fails. The Ubuntu daemon collects this in the user
journal; tune verbosity with `ATELIER_LOG`
(`RUST_LOG` syntax, default `info`). When several agents share the daemon,
every call logs a `caller=` identity: by default the TCP peer address; set an
`X-Atelier-Caller` header in a client's MCP config, or the per-call `session`
metadata below, when the name must stay stable across reconnects. (The CLI and
replay log as `cli` / `replay`.)

`doc_new` returns a fresh canonical UUIDv4 such as
`550e8400-e29b-41d4-a716-446655440000`. Its `name`
is only a display label and may repeat. Every later document call must carry
the returned `doc_id` explicitly; layer and frame targets are explicit too.
There is no active document, inferred name, CLI routing flag, or transport
default. Stdio, HTTP, CLI, and replay therefore execute exactly the same
payload.

Successful document calls include a persisted `revision`. An existing-document
mutation may pass that value as `expected_revision`; if another writer has
committed since the read, Atelier returns `revision_conflict` without running
the operation or changing its journal. Omitting the guard keeps last-write-wins
behavior. Revisions are concurrency metadata and are not recorded in replay
recipes.

MCP callers may attach one optional stable name for logs. It never supplies or
changes tool arguments:

```json
{
  "_meta": {
    "io.github.marmikshah.atelier/session": "sprite-pass"
  }
}
```

The server retains no request context. Journals record the exact arguments that
ran, so a replay never depends on a live session or another caller's state.

**25 tools**, all of them advertised — no profiles to pick, nothing hidden behind
a flag. Registry/dispatch lockstep is test-enforced, so an advertised tool cannot
turn into an unreachable dead end.
Browse them in the [tool reference](https://marmikshah.github.io/atelier/tools.html).
`list_docs` returns at most 50 documents by default (100 when requested) and
provides `next_cursor` for deterministic continuation, keeping large libraries
out of a single MCP response.

## Directory-local stores

By default everything lands in the global `~/.atelier`. Run `atelier init` in
your game or app directory and it gets its own `./.atelier`: art and recipes
live beside the code and recipes can be committed with the game. Resolution
per call: `--home` / `ATELIER_HOME` →
`./.atelier` when it exists → `~/.atelier`. Standing in `$HOME`, the two are
the same directory.

The daemon is the exception: a shared server has no working directory, so it
always pins the global store at install time (or whatever `--home` you give
it). `atelier status` shows the daemon endpoint and store.

## Backups and checkpoints

Use `atelier library pack <id> --out <file.atelierpack>` to write a complete,
deterministic document archive containing its cels, recipe, revision, stored
reference, and checkpoints. Packing never overwrites an existing file. Restore
with `atelier library unpack <file.atelierpack>`; the original document UUID is
preserved and a collision is refused. Replacing an existing document requires
both `--replace --yes`, and publication happens only after the complete archive
passes checksum and content validation. Both commands accept `--home DIR` for
an isolated store.

Explicit checkpoints remain local recovery points rather than an unbounded
history. A document may retain at most 32 checkpoints and 2 GiB of logical
checkpoint data; labels are limited to 4096 UTF-8 bytes. Atelier never silently
evicts an older checkpoint—prune one explicitly before saving another.

## Skills

Three optional [skills](crates/atelier/skills) provide higher-level workflows
for Claude Code, Codex, Kimi Code, and Cursor:

| | |
|---|---|
| **atelier-sprite** | Build a character, creature, vehicle, or prop |
| **atelier-scene** | Build a background, environment, or composed scene |
| **atelier-review** | Review completed work and propose targeted corrections |

The drawing workflows favor layered construction and localized corrections.
They do not prescribe a style or palette. Every referenced operation works
through either `atelier call` or its same-named MCP tool.

Install or refresh them any time — `atelier skills install` (Claude Code by
default, `--for codex|kimi|cursor|all` for the others).

The skills are optional and do not change the tool surface.

## Reproducible journals

Every document is an ordered sequence of tool calls, so a piece of art *is* a
replayable program — and atelier keeps that sequence for you. Every document
journals itself as it's drawn, so anything you make can rebuild itself:

```sh
atelier library                 # every document, with its step count
atelier library verify          # validate the complete store without changing it
atelier replay 550e8400-e29b-41d4-a716-446655440000  # rebuild it from its own journal
```

Nothing to turn on. The journal is JSON Lines beside the art
(`~/.atelier/documents/<id>/recipe.jsonl`), one tool call per line — only the
deterministic calls that *made* something, never looks, audits, external
reference setup, or checkpoint bookkeeping. Restoring a checkpoint restores
its journal too, so discarded edits cannot survive in provenance. Replay into
a sandbox with `--home /tmp/demo` and you get the same pixels, anywhere. The
rebuilt document is published only after every recipe step succeeds; a failed
step discards the entire staged replay.
Current journals start with exactly one `doc_new` whose arguments include the
minted `doc_id`, followed only by deterministic editing calls for that same
document. Replay accepts only this JSONL contract and rejects wrapped objects,
bindings, older shapes, or malformed lines instead of guessing.

Each `doc_draw` and `doc_fx` line applies exactly one operation. By default it
targets one cel; optional inclusive `frame_to` applies that same operation and
seed across at most 256 consecutive frame cels in one atomic call. Aggregate
full-canvas work is capped at 16,777,216 pixels. `doc_frame op=duration` likewise
uses optional `count` for at most 256 consecutive timings. `doc_paint_grid`
remains the single declarative operation for dense pixel rows. MCP clients
inspect live documents through `doc_info` and `doc_look`, the same calls used by
CLI and replay.

## Project status

Atelier currently follows a `1.y.z` release train. Feature releases increment
`y`; no 2.0 release is planned, versioned, or tagged unless the maintainer
explicitly authorizes it. Most of the implementation was generated with AI
systems and has not yet received a complete line-by-line maintainer review.
Tests, static analysis, locked dependencies, bounded image operations, and
release gates are not a substitute for that review.

> [!WARNING]
> **During the 1.x series, assume defects and breaking changes remain
> possible.** Review the code and isolate important data before using Atelier
> in a production workflow.

## Contributing

Bug reports, ideas, documentation improvements, and focused pull requests are
welcome. Open an issue first for new tools, public API changes, dependencies,
formats, or broad refactors; the full development and review expectations are in
[CONTRIBUTING.md](.github/CONTRIBUTING.md).

Maintainer releases are approved by a manually created annotated tag; the exact
procedure is in [docs/RELEASING.md](docs/RELEASING.md).
The [roadmap](docs/ROADMAP.md) records current readiness work and the policy for
adding capabilities without expanding the tool surface unnecessarily.

[Contributing](.github/CONTRIBUTING.md) · [Code of Conduct](.github/CODE_OF_CONDUCT.md) · [Security](.github/SECURITY.md) · [Changelog](CHANGELOG.md)

## License

[MIT](LICENSE) © Marmik Shah
