# atelier benchmark

Ten animation tasks, one per game-object category. Each is run by an **agent
driving atelier's tools** — nothing else. The art is the result; the tool
calls are the record.

## Tasks

Each is a **1-second loop at 10 FPS (10 frames)** on a transparent background:

| task | category |
|-------|----------|
| `person`  | character |
| `cat`     | animal |
| `car`     | vehicle |
| `alien`   | creature |
| `ball`    | prop / physics |
| `potion`  | item / pickup |
| `slash`   | effect / VFX |
| `beam`    | effect / VFX |
| `explosion` | effect / VFX |
| `torch`   | environment |

The task text is the exact input. Keep it fixed so runs stay comparable.

## How runs are made

Runs are driven by **agent subagents driving atelier's tools** — the same path
any user takes (Claude Code for the Anthropic models, Kimi Code for kimi-k3,
and Codex for gpt-5.6-sol-max), over the CLI or MCP. There is no bespoke
benchmark runner: the agent gets the task text and the tool surface, and works.

```sh
# the CLI path needs no setup at all — the agent's shell just runs:
atelier call doc_new '{"name":"cat","width":32,"height":32}'
# then passes the returned doc_id explicitly on every document call
# or, for an MCP-connected client:
atelier install && claude mcp add --transport http atelier http://127.0.0.1:8765/mcp
```

Then hand an agent one task file and let it work. Recorded per run: the model,
the tool calls it made, how many times it called `doc_look`, wall-clock, and the
exported GIF.

## Committed replays

Every model/task pair has a replay source at
`benchmarks/replays/<model>/<task>.jsonl`. These are executable journals in
Atelier's current format: minified JSON Lines, one `{tool,args}` call per line.
They deliberately use no wrapper or benchmark-only DSL, so the files exercise
the same parser and dispatch path as a user's own document journal.

The original recorded journals were migrated only where the public API changed:
document stamps use UUIDv4, `doc_create` is `doc_new`, and each old batch was
expanded into its operations in execution order. One `glow` recorded at an
intensity that rounded to no pixel changes was omitted. The resulting pixels
are unchanged.

```sh
store=$(mktemp -d)
atelier replay benchmarks/replays/haiku-4.5/alien.jsonl --home "$store"
```

`make showcase-check` replays all 60 sources through the current dispatch path,
exports their animations, and requires every GIF to match its committed
`site/showcase` counterpart byte-for-byte. The normal `make check` gate includes
this test.

## Reading the numbers

Metrics come from the client that ran the agent, so they describe **that client's
loop**, not a normalised API call — Claude Code, Kimi Code, and Codex bring their
own system prompts, context handling, and caching. Compare runs made the same
way; treat cross-client numbers as indicative, not exact. `runs.json` records
the Atelier version used for each batch when it differs.

What is exactly reproducible is the **art**: every document is an ordered
sequence of tool calls, so a recorded run replays byte-identically via
`atelier replay`.
