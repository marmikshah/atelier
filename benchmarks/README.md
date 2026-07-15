# atelier benchmark

Eight animation tasks, one per game-object category. Each is run by an **agent
driving atelier's MCP tools** — nothing else. The art is the result; the tool
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
| `torch`   | environment |

The task text is the exact input. Keep it fixed so runs stay comparable.

## How runs are made

Runs are driven through **Claude Code**, connected to atelier over MCP — the same
path any user takes. There is no bespoke benchmark runner: the agent gets the
task text and the tool surface, and works.

```sh
atelier install                  # background daemon, then connect your client
claude mcp add --transport http atelier http://127.0.0.1:8765/mcp
```

Then hand an agent one task file and let it work. Recorded per run: the model,
the tool calls it made, how many times it called `doc_look`, wall-clock, and the
exported GIF.

## Reading the numbers

Metrics come from the client that ran the agent, so they describe **that client's
loop**, not a normalised API call — Claude Code brings its own system prompt,
context handling and caching. Compare runs made the same way; treat
cross-client numbers as indicative, not exact.

What is exactly reproducible is the **art**: every document is an ordered
sequence of tool calls, so a recorded run replays byte-identically via
`atelier replay`.
