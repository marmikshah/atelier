# The atelier CLI

Every tool is available through `atelier call`: stdout contains the JSON
result, while exit codes distinguish success, tool errors, and invalid calls.
Shell-based automation needs no server registration or background process.

```sh
atelier call <tool> '<json>'          # one tool call, in-process — the front door
atelier call <tool> --file ops.json   # args from a file (or --stdin, --image-out PATH)
atelier tools [--markdown|--schema <name>]   # the surface / full reference / one schema
atelier init                          # stamp ./.atelier for a directory-local store
atelier replay <recipe|id>           # rebuild a DSL recipe or working document
atelier migrate <old|id> <new.atelier>   # import legacy JSONL or export a document recipe
atelier library                       # what's in your document store
atelier library verify [--json]       # validate metadata, cels, references, recipes
atelier library pack <id> --out art.atelierpack   # write a portable backup
atelier library unpack art.atelierpack            # restore its original UUID
atelier library rm <id>... | --prefix <p> | --all [--yes]   # delete documents, permanently
atelier library --home /tmp/store     # …and any of the above against an isolated store
atelier skills [show <name>]          # list the shipped skills, or print one
atelier skills install                # write them (--for claude|codex|kimi|cursor|all, or --dir DIR)
```

`--home DIR` selects an isolated store, and every command that touches one
accepts it: `call`, `replay`, `migrate`, and all of `library`. Without it the store is
`ATELIER_HOME`, then a directory-local `./.atelier`, then `~/.atelier`.

And the MCP add-on, covered in [mcp.md](mcp.md):

```sh
atelier                     # stdio MCP server (your client spawns it)
atelier --http              # HTTP server at 127.0.0.1:8765/mcp
atelier install             # background daemon; asks for port (default 8765)
atelier install --port 9123 # non-interactive install/reinstall
atelier status              # daemon state, endpoint, and log locations
atelier uninstall           # stop and remove the daemon
```

## Documents are addressed explicitly

`doc_new` returns a fresh canonical UUIDv4 such as
`550e8400-e29b-41d4-a716-446655440000`. Its `name` is only a display label and
may repeat. Every later document call must carry the returned `doc_id`
explicitly; layer and frame targets are explicit too.

There is no active document, inferred name, CLI routing flag, or transport
default. Stdio, HTTP, CLI therefore execute exactly the same
payload. Replay uses the same rendering operations.

## Revisions and concurrent writers

Successful document calls include a persisted `revision`. An existing-document
mutation may pass that value as `expected_revision`; if another writer has
committed since the read, Atelier returns `revision_conflict` without running
the operation or changing its recipe. Omitting the guard keeps last-write-wins
behavior. Revisions are concurrency metadata and are not recorded in replay
recipes.

## The tool surface

**25 tools**, all of them advertised — no profiles to pick, nothing hidden
behind a flag. Registry/dispatch lockstep is test-enforced, so an advertised
tool cannot turn into an unreachable dead end. Browse them in the
[tool reference](tools.md).

`list_docs` returns at most 50 documents by default (100 when requested) and
provides `next_cursor` for deterministic continuation, keeping large libraries
out of a single MCP response.

Image-producing inspection calls return pixels only when requested. Editing
calls return compact structured results, which keeps automated feedback loops
predictable without embedding a preview after every mutation. Inline MCP PNGs
are limited to 8 MiB before base64 encoding; larger renders already written to
`out_path` return their report without a duplicate inline image, while other
oversized renders fail with reduction/output guidance.

Each `doc_draw` and `doc_fx` call applies exactly one operation. By default it
targets one cel; optional inclusive `frame_to` applies that same operation and
seed across at most 256 consecutive frame cels in one atomic call. Aggregate
full-canvas work is capped at 16,777,216 pixels. `doc_frame op=duration`
likewise uses optional `count` for at most 256 consecutive timings.
`doc_paint_grid` remains the single declarative operation for dense pixel rows.
MCP clients inspect live documents through `doc_info` and `doc_look`, the same
calls used by CLI and replay.

## Where documents live

By default everything lands in the global `~/.atelier`. Run `atelier init` in
your game or app directory and it gets its own `./.atelier`: art and recipes
live beside the code, and recipes can be committed with the game. Resolution
per call:

```text
--home / ATELIER_HOME  →  ./.atelier when it exists  →  ~/.atelier
```

Standing in `$HOME`, the two are the same directory.

The daemon is the exception: a shared server has no working directory, so it
always pins the global store at install time (or whatever `--home` you give
it). `atelier status` shows the daemon endpoint and store.

## Structured replay files

Normal calls automatically update `documents/<id>/recipe/recipe.atelier` inside
the selected store. The [recipe format](recipes.md) groups current artwork by
layer and frame, with document settings written once. It has no working UUIDs,
tool-call envelopes, or edit history. Repeated pixels are stored once when
multiple frames of a layer share the same cel.

```sh
atelier library                         # documents and current recipe sizes
atelier library verify                  # verify pixels, metadata and replay agree
atelier replay <doc-id>                  # rebuild that document's current recipe
atelier replay art/lantern/recipe.atelier --home target/art-review
atelier migrate <doc-id> art/lantern.atelier # export an editable recipe
atelier migrate old.jsonl art/lantern.atelier # import a legacy command log once
atelier migrate --store --home .atelier # preserve ids while converting live recipes
```

A standalone `.atelier` file contains the complete editable artwork. A recipe
with a reference image exports as a directory holding `recipe.atelier` and that
reference. Small sprites use grids; large regions use colour rows or occupied
strips. Drawing commands remain editable when later paint patches are added.
The working store materializes pixels for its readers; editing the source and
replaying it rebuilds those pixels.

`replay` accepts structured recipes only. `migrate` is the sole legacy JSONL
entry point; it verifies metadata, cel bounds, offsets and every RGBA pixel,
writes to a new directory, and leaves the input untouched. Editing an older
working document also replaces its live JSONL with a structured recipe.
Checkpoint restore brings back the matching recipe and pixels.

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
evicts an older checkpoint — prune one explicitly before saving another.

## Skills

Three optional [skills](../crates/atelier/skills) provide higher-level
workflows for Claude Code, Codex, Kimi Code, and Cursor:

| | |
|---|---|
| **atelier-sprite** | Build a character, creature, vehicle, or prop |
| **atelier-scene** | Build a background, environment, or composed scene |
| **atelier-review** | Review completed work and propose targeted corrections |

The drawing workflows favor layered construction and localized corrections.
They do not prescribe a style or palette. Every referenced operation works
through either `atelier call` or its same-named MCP tool.

Install or refresh them any time — `atelier skills install` (Claude Code by
default, `--for codex|kimi|cursor|all` for the others). The skills are optional
and do not change the tool surface.
