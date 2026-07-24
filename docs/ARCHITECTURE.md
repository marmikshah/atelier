# Architecture

Atelier is an offline pixel-art editor with one document model and three ways
to drive it: the CLI, MCP, and recipe replay.

## Dependency direction

```text
atelier-core → atelier-studio → atelier-mcp → atelier
```

- **`atelier-core`** owns pixels: documents, layers, frames, tags, palette
  operations, drawing, effects, analysis, and file encoding. It has no async,
  MCP, CLI, or store policy.
- **`atelier-studio`** owns the on-disk document library. It opens one document,
  calls core, saves it, and returns JSON-shaped reports. It also owns
  per-document journals and explicit checkpoints.
- **`atelier-mcp`** owns the 26 public tool schemas, shared dispatch, MCP
  resources, and stdio/HTTP transports.
- **`atelier`** is the executable: CLI routing, local calls, replay, local-store
  initialization, daemon management, library cleanup, and skill installation.

Dependencies point only to the left. A lower layer must not know how a higher
layer exposes it.

## One dispatch path

```text
CLI call ─┐
replay ───┼─→ Atelier::dispatch
MCP ──────┘        │
                   ├─ classify read/write
                   ├─ acquire store lock
                   ├─ deserialize the tool parameters
                   ├─ call Studio
                   ├─ log the result
                   └─ append successful mutations to recipe.jsonl
```

The MCP router advertises schemas; `Atelier::dispatch` invokes handlers. Tests
require every advertised tool to have a dispatch arm.

Mutations are serialized twice:

- an async order lock coordinates concurrent requests within one server;
- an advisory file lock coordinates independent CLI and daemon processes that
  resolve to the same store.

The lock is held through the journal append, so document state and provenance
cannot be committed in different orders.

## Storage

The default store is `~/.atelier`. `ATELIER_HOME` overrides it. A directory
containing `.atelier` opts into a local store, which `atelier init` creates
explicitly.

```text
.atelier/
└── documents/
    └── hero/
        ├── doc.json
        ├── recipe.jsonl
        ├── cels/
        │   ├── L0_F0.png
        │   └── L1_F0.png
        └── .checkpoints/
```

`doc.json` contains structure and cel references. Cel PNGs hold pixels.
`recipe.jsonl` is append-only, one ordinary `{tool,args}` object per line.
Replay also accepts an authored JSON object with `{name,description,steps}`.
There is no compact codec or project build manifest.

## Tool organization

The public surface is deliberately small and fully advertised:

- document/library structure;
- drawing, effects, grids, and stateless region edits;
- visual inspection and animation audits;
- palette and reference workflows;
- per-document sheet and animation export.

Single-op drawing tools and `doc_batch` share the same core operation registry,
validation, and execution. Adding a new public tool requires a schema, handler,
dispatch arm, documentation update, and tests.

## Transports

The CLI needs no transport:

```sh
atelier call doc_look '{"doc_id":"hero"}'
```

MCP is optional:

- bare `atelier` serves stdio;
- `atelier --http ADDR` serves stateless Streamable HTTP at `/mcp`;
- `atelier install` manages the HTTP server through launchd or systemd user
  services.

Atelier does not edit third-party MCP client configuration. Clients point at
the HTTP endpoint printed by `atelier status`, or spawn `atelier` over stdio.

## Verification

`make check` is the complete gate:

- workspace metadata and lockfile consistency;
- formatting, Clippy, and rustdoc;
- unit and integration tests;
- replay of every example recipe;
- generated tool-reference validation.

Release builds and CI use the committed `Cargo.lock`. Only the maintainer tags
and publishes releases.
